//! Interface gpui.
//!
//! Seule cette couche connaît gpui. Elle ne fait jamais d'entrée-sortie
//! elle-même : elle envoie des `Cmd` aux workers git (`crate::runtime`) et
//! réagit aux `Evt` qu'ils renvoient, drainés par une tâche gpui de premier
//! plan. Les terminaux ont leur propre boucle, dans `crate::terminal`.

mod app;
mod branches;
mod diff_view;
mod highlight;
mod history_view;
mod icons;
mod review;
mod settings;
mod shortcuts;
mod sidebar;
mod terminal_view;
mod theme;

use gpui::{px, size, App, AppContext, Application, Bounds, WindowBounds, WindowOptions};
use gpui_component::Root;
use rust_embed::RustEmbed;

pub use settings::{LanguageChoice, Settings, ThemeMode};

/// Interface et texte courant.
const INTER_FONT: &[u8] = include_bytes!("../../assets/fonts/Inter.ttf");
const INTER_BOLD_FONT: &[u8] = include_bytes!("../../assets/fonts/Inter-Bold.ttf");
/// Terminaux, diffs, chemins de fichiers : tout ce qui doit s'aligner en
/// colonnes. Le rendu du terminal suppose une chasse fixe.
const JETBRAINS_MONO_FONT: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono.ttf");

/// Assets embarqués dans le binaire et servis à gpui.
///
/// Les polices en sont exclues : elles arrivent par `include_bytes!`
/// ci-dessus, et les lister ici les mettrait une seconde fois dans le
/// binaire.
#[derive(RustEmbed)]
#[folder = "assets"]
#[include = "icons/**/*.svg"]
struct Assets;

impl gpui::AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        Ok(Self::get(path).map(|f| f.data))
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<gpui::SharedString>> {
        Ok(Self::iter()
            .filter(|p| p.starts_with(path))
            .map(|p| p.to_string().into())
            .collect())
    }
}

/// Chaque catalogue `rust-i18n` a son propre état de langue : gpui-component a
/// le sien pour ses menus intégrés, et il faut le régler aussi, sinon la
/// moitié de l'interface change de langue et l'autre non.
pub(crate) fn set_language(language: LanguageChoice) {
    let locale = language.to_lang_id();
    rust_i18n::set_locale(locale);
    gpui_component::set_locale(locale);
}

fn install_fonts(cx: &mut App) {
    let fonts = [INTER_FONT, INTER_BOLD_FONT, JETBRAINS_MONO_FONT];
    if let Err(e) = cx
        .text_system()
        .add_fonts(fonts.into_iter().map(std::borrow::Cow::Borrowed).collect())
    {
        log::warn!("polices embarquées non chargées : {e:#}");
    }
}

pub fn run() {
    let settings = Settings::load();
    set_language(settings.language);

    Application::new().with_assets(Assets).run(move |cx| {
        gpui_component::init(cx);
        highlight::register_languages();
        shortcuts::init(cx);
        install_fonts(cx);
        theme::apply(&settings, None, cx);

        let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
        // `Maximized` porte quand même ces dimensions : elles deviennent la
        // taille de restauration.
        let window_bounds = if settings.start_maximized {
            WindowBounds::Maximized(bounds)
        } else {
            WindowBounds::Windowed(bounds)
        };
        cx.activate(true);

        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(window_bounds),
                titlebar: Some(gpui_component::TitleBar::title_bar_options()),
                app_id: Some("perch".into()),
                ..Default::default()
            },
            |window, cx| {
                let main = cx.new(|cx| app::PerchApp::new(settings.clone(), window, cx));
                cx.new(|cx| Root::new(main, window, cx))
            },
        );
        if let Err(e) = opened {
            log::error!("ouverture de la fenêtre : {e:#}");
        }
    });
}

#[cfg(test)]
mod i18n_tests {
    //! Les deux catalogues doivent rester interchangeables : une clé présente
    //! d'un côté et pas de l'autre se voit à l'exécution sous la forme du nom
    //! de la clé affiché à la place du texte, ce qu'aucune relecture ne
    //! rattrape de façon fiable.

    const EN: &str = include_str!("../../assets/i18n/en.json");
    const FR: &str = include_str!("../../assets/i18n/fr.json");

    fn keys(json: &str) -> std::collections::BTreeSet<String> {
        let value: serde_json::Value = serde_json::from_str(json).expect("catalogue JSON valide");
        value
            .as_object()
            .expect("catalogue plat clé → chaîne")
            .keys()
            .cloned()
            .collect()
    }

    #[test]
    fn both_catalogs_have_the_same_keys() {
        let (en, fr) = (keys(EN), keys(FR));
        let missing_fr: Vec<_> = en.difference(&fr).collect();
        let missing_en: Vec<_> = fr.difference(&en).collect();
        assert!(
            missing_fr.is_empty() && missing_en.is_empty(),
            "absentes de fr.json : {missing_fr:?}\nabsentes de en.json : {missing_en:?}"
        );
    }

    #[test]
    fn both_catalogs_use_the_same_placeholders() {
        let en: serde_json::Value = serde_json::from_str(EN).unwrap();
        let fr: serde_json::Value = serde_json::from_str(FR).unwrap();
        let placeholders = |s: &str| -> std::collections::BTreeSet<String> {
            let mut set = std::collections::BTreeSet::new();
            let mut rest = s;
            while let Some(start) = rest.find("%{") {
                let after = &rest[start + 2..];
                match after.find('}') {
                    Some(end) => {
                        set.insert(after[..end].to_string());
                        rest = &after[end + 1..];
                    }
                    None => break,
                }
            }
            set
        };
        for (key, en_value) in en.as_object().unwrap() {
            let Some(fr_value) = fr.get(key) else {
                continue;
            };
            assert_eq!(
                placeholders(en_value.as_str().unwrap_or_default()),
                placeholders(fr_value.as_str().unwrap_or_default()),
                "les substitutions de « {key} » diffèrent entre les deux langues"
            );
        }
    }
}
