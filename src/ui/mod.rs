//! The gpui interface.
//!
//! Only this layer knows gpui. It never does I/O itself: it sends `Cmd`s to
//! the git workers (`crate::runtime`) and reacts to the `Evt`s they send back,
//! drained by a foreground gpui task. Terminals have their own loop, in
//! `crate::terminal`.

mod app;
mod base_select;
mod blade;
mod branches;
mod conflicts;
mod db;
mod db_query;
mod dialogs;
mod diff_view;
mod explorer;
mod file_icons;
mod find;
mod folds;
mod highlight;
mod history_view;
mod icons;
mod inflight;
mod jumps;
mod lsp;
mod motion;
mod notes;
pub(crate) mod notes_view;
mod notify;
mod panels;
pub mod plugin_view;
mod repos;
mod review;
mod scroll;
mod search;
mod search_view;
mod sentry_view;
mod server;
mod session;
mod settings;
mod settings_view;
mod shortcuts;
mod shortcuts_view;
mod sql_history;
mod sql_history_view;
mod store;
mod tags;
mod terminal_view;
mod theme;
mod topbar;
mod tree;
mod vault;
mod vim;
mod workspace;
mod worktree_ops;

use gpui::{px, size, App, AppContext, Bounds, WindowBounds, WindowOptions};
use gpui_component::Root;
use rust_embed::RustEmbed;

pub use settings::{split_command, LanguageChoice, Settings, ThemeMode};

/// Interface and body text, the default: Iosevka's quasi-proportional cut.
const IOSEVKA_AILE_FONT: &[u8] = include_bytes!("../../assets/fonts/IosevkaAile.ttf");
const IOSEVKA_AILE_BOLD_FONT: &[u8] = include_bytes!("../../assets/fonts/IosevkaAile-Bold.ttf");
/// Terminals, diffs, file paths: everything that has to line up in columns.
/// The terminal's rendering assumes a fixed pitch, hence the `Mono` cut and not
/// the plain Nerd Font one, whose `@` and `W` are a cell and a half wide.
const IOSEVKA_MONO_FONT: &[u8] = include_bytes!("../../assets/fonts/IosevkaNerdFontMono.ttf");
const IOSEVKA_MONO_BOLD_FONT: &[u8] =
    include_bytes!("../../assets/fonts/IosevkaNerdFontMono-Bold.ttf");
/// The former defaults. They stay embedded so that a `settings.json` naming
/// them keeps working on a machine that never installed them.
const INTER_FONT: &[u8] = include_bytes!("../../assets/fonts/Inter.ttf");
const INTER_BOLD_FONT: &[u8] = include_bytes!("../../assets/fonts/Inter-Bold.ttf");
const JETBRAINS_MONO_FONT: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono.ttf");

/// Assets embedded in the binary and served to gpui.
///
/// Fonts are excluded: they arrive through `include_bytes!` above, and listing
/// them here would put them in the binary a second time.
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

/// Each `rust-i18n` catalogue has its own locale state: gpui-component has one
/// for its built-in menus, and it has to be set too, otherwise half the
/// interface changes language and the other half does not.
pub(crate) fn set_language(language: LanguageChoice) {
    let locale = language.to_lang_id();
    rust_i18n::set_locale(locale);
    gpui_component::set_locale(locale);
}

fn install_fonts(cx: &mut App) {
    let fonts = [
        IOSEVKA_AILE_FONT,
        IOSEVKA_AILE_BOLD_FONT,
        IOSEVKA_MONO_FONT,
        IOSEVKA_MONO_BOLD_FONT,
        INTER_FONT,
        INTER_BOLD_FONT,
        JETBRAINS_MONO_FONT,
    ];
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
            install_fonts(cx);
            // The settings become global before anything else: the form writes
            // them from closures that only have an `App`, and installing the
            // themes reads them back to know what to apply.
            settings.clone().init_global(cx);
            // The keymap comes after them — it reads the shortcuts one has
            // customised — and after `gpui_component::init`, whose own bindings
            // it takes a snapshot of: see `shortcuts::init`.
            shortcuts::init(cx);
            // Per-worktree state follows the settings: the views read it just
            // the same, and it has to be there before the first of them.
            store::Store::load().init_global(cx);
            // Before the window: a plugin's panel has to be in the dock's
            // registry before `layout.json` is read back, or its tab comes back
            // as an empty frame. That is why adding or removing a plugin takes
            // a restart, while its script reloads hot.
            plugin_view::install();
            theme::install(cx);
            theme::apply(&settings, None, cx);

            let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
            // `Maximized` carries these dimensions anyway: they become the
            // restore size.
            let window_bounds = if settings.start_maximized {
                WindowBounds::Maximized(bounds)
            } else {
                WindowBounds::Windowed(bounds)
            };
            cx.activate(true);

            let opened = cx.open_window(
                // `TitleBar::window_options` rather than `Default`: it also
                // sets `app_owns_titlebar_drag`, without which the platform
                // and our bar fight over the double-click and the drag. It is
                // `topbar::render_topbar` that draws the bar — without it, the
                // window has nothing left to be moved or closed by.
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
