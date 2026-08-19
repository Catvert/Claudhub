//! Thème.
//!
//! Perch reprend les palettes claire et sombre de gpui-component et n'y touche
//! qu'à la marge : les couleurs des diffs et des états git, que la
//! bibliothèque n'a pas de raison de connaître.

use std::path::PathBuf;

use gpui::{px, App, Hsla, Pixels, Rgba, Window};
use gpui_component::{Theme, ThemeRegistry};

use super::settings::{Settings, ThemeMode};

/// Les thèmes livrés avec Perch.
///
/// Ils sont embarqués dans le binaire, puis **écrits sur le disque** au
/// démarrage : le registre de gpui-component ne se charge que depuis un
/// répertoire, qu'il surveille. L'effet de bord est heureux — le même
/// répertoire accueille les thèmes que l'utilisateur ajoute, et un fichier
/// modifié est rechargé sans relancer Perch.
#[derive(rust_embed::RustEmbed)]
#[folder = "assets/themes"]
#[include = "*.json"]
struct BundledThemes;

pub fn themes_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("be", "acetics", "perch")
        .map(|dirs| dirs.config_dir().join("themes"))
}

/// Installe les thèmes livrés et met le registre à leur écoute.
///
/// Les fichiers `perch-*.json` sont réécrits à chaque démarrage : c'est ce qui
/// fait qu'une mise à jour de Perch corrige un thème sans demander une
/// manœuvre. Pour en modifier un, il faut donc le copier sous un autre nom —
/// tout fichier `.json` du répertoire est chargé.
pub fn install(cx: &mut App) {
    let Some(dir) = themes_dir() else {
        return;
    };
    if let Err(e) = write_bundled(&dir) {
        log::warn!("thèmes non installés : {e}");
    }
    // Le chargement est asynchrone : le thème choisi n'existe pas encore dans
    // le registre à cet instant, d'où la ré-application dans le rappel.
    let result = ThemeRegistry::watch_dir(dir, cx, |cx| {
        let settings = Settings::global(cx).clone();
        apply(&settings, None, cx);
    });
    if let Err(e) = result {
        log::warn!("répertoire de thèmes non surveillé : {e}");
    }
}

fn write_bundled(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for name in BundledThemes::iter() {
        let Some(file) = BundledThemes::get(&name) else {
            continue;
        };
        std::fs::write(dir.join(name.as_ref()), file.data)?;
    }
    Ok(())
}

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

    // Les palettes nommées, prises dans le registre. Un nom inconnu — un
    // fichier supprimé, un réglage écrit à la main — laisse le thème en place
    // plutôt que de repeindre la fenêtre en blanc sans explication.
    let registry = ThemeRegistry::global(cx);
    let light = registry
        .themes()
        .get(settings.light_theme.as_str())
        .cloned();
    let dark = registry.themes().get(settings.dark_theme.as_str()).cloned();

    // `Theme::change` crée le global s'il manque : il faut passer par lui
    // avant de pouvoir écrire dans les deux emplacements.
    Theme::change(mode, None, cx);
    let theme = Theme::global_mut(cx);
    if let Some(light) = light {
        theme.light_theme = light;
    }
    if let Some(dark) = dark {
        theme.dark_theme = dark;
    }

    // `Theme::change` réinitialise les couleurs : tout réglage de palette doit
    // venir après, sinon il est effacé sans bruit.
    Theme::change(mode, window, cx);

    let theme = Theme::global_mut(cx);
    theme.font_family = settings.ui_font().to_string().into();
    theme.mono_font_family = settings.mono_font().to_string().into();
    theme.font_size = px(settings.font_size);

    cx.refresh_windows();
}

/// Hauteur d'une ligne de liste.
///
/// Elle se déduit de la taille du texte au lieu d'être écrite en dur : une
/// hauteur figée déborde dès qu'on grossit la police, et les listes
/// virtualisées ne mesurent rien — elles réservent exactement ce qu'on leur
/// annonce, si bien qu'une ligne trop haute recouvre la suivante.
pub fn row_height(cx: &App) -> Pixels {
    scaled(cx, 1.9, px(22.))
}

/// Hauteur d'une ligne de liste qui porte deux étages de texte.
///
/// Une hauteur d'une seule ligne y ferait déborder le second étage sur la
/// ligne suivante — les listes virtualisées ne mesurent rien et réservent
/// exactement ce qu'on leur annonce.
pub fn tall_row_height(cx: &App) -> Pixels {
    scaled(cx, 3.0, px(34.))
}

/// Hauteur d'une barre d'en-tête : onglets, titres de panneaux.
pub fn bar_height(cx: &App) -> Pixels {
    scaled(cx, 2.2, px(26.))
}

/// Hauteur de la barre d'outils principale, qui porte des boutons.
pub fn toolbar_height(cx: &App) -> Pixels {
    scaled(cx, 2.7, px(32.))
}

fn scaled(cx: &App, factor: f32, floor: Pixels) -> Pixels {
    let base = gpui_component::ActiveTheme::theme(cx).font_size;
    (base * factor).round().max(floor)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Un thème que le registre n'arrive pas à lire est simplement ignoré :
    /// aucune erreur ne remonte à l'écran, il manque juste de la liste. Ce
    /// test est le seul endroit où une faute de frappe dans un JSON se voit.
    #[test]
    fn every_bundled_theme_parses() {
        let mut count = 0;
        for name in BundledThemes::iter() {
            let file = BundledThemes::get(&name).expect("fichier embarqué");
            let text = std::str::from_utf8(&file.data).expect("UTF-8");
            let set: gpui_component::ThemeSet =
                serde_json::from_str(text).unwrap_or_else(|e| panic!("{name} illisible : {e}"));
            assert!(!set.themes.is_empty(), "{name} ne déclare aucun thème");
            for theme in &set.themes {
                assert!(!theme.name.is_empty(), "{name} : un thème sans nom");
            }
            count += 1;
        }
        assert!(count >= 10, "les thèmes livrés ont disparu du binaire");
    }

    /// Une clé absente ne provoque pas d'erreur : elle reprend la valeur par
    /// défaut, qui est *claire*. Sur un thème sombre, cela fait une tache
    /// blanche au milieu de la fenêtre, et rien ne le signale.
    #[test]
    fn no_bundled_theme_leaves_a_colour_unset() {
        let reference: std::collections::BTreeSet<String> = keys_of("perch-nord.json");
        for name in BundledThemes::iter() {
            let keys = keys_of(&name);
            let missing: Vec<_> = reference.difference(&keys).collect();
            assert!(missing.is_empty(), "{name} : couleurs absentes {missing:?}");
        }
    }

    fn keys_of(name: &str) -> std::collections::BTreeSet<String> {
        let file = BundledThemes::get(name).expect("fichier embarqué");
        let value: serde_json::Value = serde_json::from_slice(&file.data).expect("JSON");
        value["themes"][0]["colors"]
            .as_object()
            .expect("une table de couleurs")
            .keys()
            .cloned()
            .collect()
    }
}
