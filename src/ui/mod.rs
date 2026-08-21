//! Interface gpui.
//!
//! Seule cette couche connaît gpui. Elle ne fait jamais d'entrée-sortie
//! elle-même : elle envoie des `Cmd` aux workers git (`crate::runtime`) et
//! réagit aux `Evt` qu'ils renvoient, drainés par une tâche gpui de premier
//! plan. Les terminaux ont leur propre boucle, dans `crate::terminal`.

mod app;
mod base_select;
mod blade;
mod branches;
mod conflicts;
mod db;
mod db_query;
mod diff_view;
mod explorer;
mod file_icons;
mod find;
mod highlight;
mod history_view;
mod icons;
mod motion;
mod notes;
pub(crate) mod notes_view;
mod panels;
mod review;
mod scroll;
mod sentry_view;
mod server;
mod settings;
mod settings_view;
mod shortcuts;
mod shortcuts_view;
mod sidebar;
mod store;
mod terminal_view;
mod theme;
mod tree;
mod vault;
mod workspace;
mod worktree_ops;

use gpui::{px, size, App, AppContext, Bounds, WindowBounds, WindowOptions};
use gpui_component::Root;
use rust_embed::RustEmbed;

pub use settings::{split_command, LanguageChoice, Settings, ThemeMode};

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
pub(crate) struct Assets;

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
        log::warn!("embedded fonts not loaded: {e:#}");
    }
}

pub fn run() {
    // Before any read: what the user configured under the project's former
    // name is picked up here, otherwise they would find a brand-new window.
    settings::migrate_from_perch();
    let settings = Settings::load();
    set_language(settings.language);

    // `gpui_platform::application()` replaced `Application::new()`: gpui was
    // split in two, the core on one side and the platform (wayland, x11,
    // font-kit) on the other, and the latter is what builds the loop.
    gpui_platform::application()
        .with_assets(Assets)
        .run(move |cx| {
            gpui_component::init(cx);
            highlight::register_languages();
            shortcuts::init(cx);
            install_fonts(cx);
            // Les réglages passent en global avant tout le reste : le formulaire
            // les écrit depuis des fermetures qui n'ont qu'un `App`, et
            // l'installation des thèmes les relit pour savoir quoi appliquer.
            settings.clone().init_global(cx);
            // L'état par worktree suit les réglages : les vues le lisent au même
            // titre, et il doit être là avant la première d'entre elles.
            store::Store::load().init_global(cx);
            theme::install(cx);
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
                // `TitleBar::window_options` et non `Default` : elle pose
                // aussi `app_owns_titlebar_drag`, sans quoi la plateforme et
                // notre barre se disputent le double-clic et le glissement.
                // C'est `app::render_topbar` qui dessine la barre — sans elle,
                // la fenêtre n'a plus rien pour être déplacée ni fermée.
                WindowOptions {
                    window_bounds: Some(window_bounds),
                    app_id: Some("claudhub".into()),
                    ..gpui_component::TitleBar::window_options()
                },
                |window, cx| {
                    let main = cx.new(|cx| app::ClaudhubApp::new(window, cx));
                    cx.new(|cx| Root::new(main, window, cx))
                },
            );
            if let Err(e) = opened {
                log::error!("opening the window: {e:#}");
            }
        });
}

#[cfg(test)]
mod i18n_tests {
    //! The two catalogues must stay interchangeable: a key present on one side
    //! and not the other shows up at run time as the key name displayed
    //! instead of the text, which no review catches reliably.

    const EN: &str = include_str!("../../assets/i18n/en.json");
    const FR: &str = include_str!("../../assets/i18n/fr.json");

    fn keys(json: &str) -> std::collections::BTreeSet<String> {
        let value: serde_json::Value = serde_json::from_str(json).expect("valid JSON catalogue");
        value
            .as_object()
            .expect("flat key → string catalogue")
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
            "missing from fr.json: {missing_fr:?}\nmissing from en.json: {missing_en:?}"
        );
    }

    /// The views menu renders the raw key when it is missing, and an entry
    /// named "panel-sentry" in the middle of the menu is all you would see.
    #[test]
    fn every_view_has_a_title_in_both_catalogs() {
        let (en, fr) = (keys(EN), keys(FR));
        for workspace in crate::ui::workspace::Workspace::ALL {
            let label = workspace.label();
            assert!(en.contains(label), "missing from en.json: {label}");
            assert!(fr.contains(label), "missing from fr.json: {label}");
            for (_, title) in workspace.views() {
                assert!(en.contains(*title), "missing from en.json: {title}");
                assert!(fr.contains(*title), "missing from fr.json: {title}");
            }
        }
    }

    /// The status bar renders the raw key when it is missing: "running-push"
    /// where "Pushing…" was meant, in the one place one looks to know whether
    /// anything is happening.
    #[test]
    fn every_action_has_its_two_messages_in_both_catalogs() {
        let (en, fr) = (keys(EN), keys(FR));
        for action in crate::runtime::Action::ALL {
            for key in [action.success_key(), action.running_key()] {
                assert!(en.contains(key), "missing from en.json: {key}");
                assert!(fr.contains(key), "missing from fr.json: {key}");
            }
        }
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
                "the placeholders of \"{key}\" differ between the two languages"
            );
        }
    }
}
