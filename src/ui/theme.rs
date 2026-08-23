//! Theme.
//!
//! Claudhub takes gpui-component's light and dark palettes and only touches
//! them at the margins: the colours of diffs and git states, which the library
//! has no reason to know about.

use std::path::PathBuf;

use gpui::{prelude::*, px, App, Hsla, Pixels, Rgba, Window};
use gpui_component::{Theme, ThemeRegistry};

use super::settings::{Settings, ThemeMode};

/// The themes shipped with Claudhub.
///
/// They are embedded in the binary, then **written to disk** at startup:
/// gpui-component's registry only loads from a directory, which it watches.
/// The side effect is a happy one — the same directory holds the themes the
/// user adds, and a modified file is reloaded without restarting Claudhub.
#[derive(rust_embed::RustEmbed)]
#[folder = "assets/themes"]
#[include = "*.json"]
pub(super) struct BundledThemes;

pub fn themes_dir() -> Option<PathBuf> {
    super::settings::config_dir().map(|dir| dir.join("themes"))
}

/// Installs the bundled themes and points the registry at them.
///
/// The `claudhub-*.json` files are rewritten on every startup: that is what
/// makes a Claudhub update fix a theme without asking for a manoeuvre. To
/// modify one, it therefore has to be copied under another name — every
/// `.json` file in the directory is loaded.
pub fn install(cx: &mut App) {
    let Some(dir) = themes_dir() else {
        return;
    };
    if let Err(e) = write_bundled(&dir) {
        log::warn!("themes not installed: {e}");
    }
    // Loading is asynchronous: the chosen theme does not exist in the registry
    // yet at this point, hence the re-application in the callback.
    let result = ThemeRegistry::watch_dir(dir, cx, |cx| {
        let settings = Settings::global(cx).clone();
        apply(&settings, None, cx);
    });
    if let Err(e) = result {
        log::warn!("theme directory not watched: {e}");
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

/// Applies the chosen theme. To be called at startup and on every change of
/// mode or text size.
pub fn apply(settings: &Settings, window: Option<&mut Window>, cx: &mut App) {
    let mode = match settings.theme {
        ThemeMode::Dark => gpui_component::ThemeMode::Dark,
        ThemeMode::Light => gpui_component::ThemeMode::Light,
        // gpui does not expose the system preference portably; dark mode is a
        // development tool's default.
        ThemeMode::System => match cx.window_appearance() {
            gpui::WindowAppearance::Light | gpui::WindowAppearance::VibrantLight => {
                gpui_component::ThemeMode::Light
            }
            _ => gpui_component::ThemeMode::Dark,
        },
    };

    // The named palettes, taken from the registry. An unknown name — a deleted
    // file, a hand-written setting — leaves the theme in place rather than
    // repainting the window white without explanation.
    let registry = ThemeRegistry::global(cx);
    let light = registry
        .themes()
        .get(settings.light_theme.as_str())
        .cloned();
    let dark = registry.themes().get(settings.dark_theme.as_str()).cloned();

    // `Theme::change` creates the global if it is missing: it has to be called
    // before either slot can be written.
    Theme::change(mode, None, cx);
    let theme = Theme::global_mut(cx);
    if let Some(light) = light {
        theme.light_theme = light;
    }
    if let Some(dark) = dark {
        theme.dark_theme = dark;
    }

    // `Theme::change` resets the colours: any palette tweak has to come after,
    // or it is wiped silently.
    Theme::change(mode, window, cx);

    let theme = Theme::global_mut(cx);
    theme.font_family = settings.ui_font().to_string().into();
    theme.mono_font_family = settings.mono_font().to_string().into();
    theme.font_size = px(settings.font_size);
    // Scrollbars are **always shown**.
    //
    // Neither `Scrolling`, the default, which hides them two seconds after the
    // last wheel notch: a several-thousand-line diff is read stopping at every
    // hunk, and the bar — the only positional cue a virtualised list has —
    // would have vanished each time you wonder where you are. Nor `Hover`,
    // which only shows them once the pointer is on the bar: it is invisible,
    // therefore unfindable.
    theme.scrollbar_mode = gpui_component::scroll::ScrollbarMode::Always;

    // gpui-component's radii are a 2015 web form's: six and eight pixels. They
    // carry everything the window shows — buttons, fields, menus, dialogs — and
    // moving them up a notch is enough to change the grain of the whole
    // application.
    //
    // Only if the palette does not take care of it: `radius`, `radius.lg` and
    // `shadow` are `ThemeConfig` keys, and a theme that declares them has chosen
    // its grain — overwriting it would amount to never having read them.
    let config = if theme.mode.is_dark() {
        theme.dark_theme.clone()
    } else {
        theme.light_theme.clone()
    };
    if config.radius.is_none() {
        theme.radius = px(8.);
    }
    if config.radius_lg.is_none() {
        theme.radius_lg = px(12.);
    }

    // The tab bar takes the gutter's colour, and the active tab the colour of
    // the card it opens.
    //
    // This is the division of roles, which is ours — the colours stay the
    // palette's: `TabPanel` paints its bar in `tab_bar` and its body in
    // `background`, and while the two were close, the window was nothing but a
    // grid of stitched rectangles. Blended into the gutter, the bar lets tabs
    // read as sitting on a card — which the library could not do by itself, its
    // `TabPanel::render` being a `size_full()` with neither radius nor margin.
    let base = theme.background;
    let gutter = Hsla {
        l: (base.l - 0.05).max(0.),
        ..base
    };
    theme.tab_bar = gutter;
    // The tab rail is **on the card**, not on the gutter: a group's bar and
    // content share the same surface, and that is what blends them into each
    // other — the old seam between the two was precisely the bar painted in the
    // gutter colour above a bordered card.
    theme.tab_bar_segmented = base;
    // The active pill therefore carries a **raised** tone (that of section
    // headers), not the card's — on the card it would be invisible.
    theme.tab_active = theme.secondary;
    // The active tab is a block of the card's colour: its label must therefore
    // carry the ordinary text colour, and the others stay set back — otherwise
    // all five tabs read the same and the active one is told apart only by a
    // background.
    theme.tab_active_foreground = theme.foreground;
    theme.tab_foreground = theme.muted_foreground;
    theme.tab = gutter;

    // **The tokens are recomputed afterwards.** `Theme::tokens` is derived from
    // the colours, but only once, when the palette is applied: recent
    // components — the dock's tab bar among them — read `tokens` and not
    // `colors`, and a colour changed here without this call shows up nowhere.
    // That is this version's silent failure.
    theme.tokens = gpui_component::ThemeTokens::from(&theme.colors);

    // The base layer keeps **its own copy** of the theme — resize handles and
    // scrollbars paint without going through gpui-component. Without this
    // projection, none of the above reaches it; and since it rebuilds the copy
    // from scratch, the handle tweak comes after.
    Theme::sync_base(cx);
    let base = gpui_base::Theme::global_mut(cx);
    // The handle at rest paints nothing: the cards are already separated by the
    // gutter, and a grey line on top would stitch back what we have just
    // unstitched. It stays grabbable — the grab zone is wider than the line —
    // and shows during the drag, where it is information.
    base.resizable.handle = gpui::transparent_black();

    cx.refresh_windows();
}

/// The height of a list row.
///
/// It is derived from the text size instead of being hard-coded: a fixed
/// height overflows as soon as the font grows, and virtualised lists measure
/// nothing — they reserve exactly what they are told, so a too-tall row covers
/// the next one.
pub fn row_height(cx: &App) -> Pixels {
    scaled(cx, 1.9, px(22.))
}

/// What a row keeps free on its right, for the scrollbar.
///
/// The bar is painted **over** the content and not beside it — see
/// `ui::scroll` — so a row that runs to the panel's edge runs under it, and
/// what sits at the end of a row is precisely what one reads at a glance: the
/// review tick, the status letter, the `+n −m`. The band itself still crosses
/// the whole panel; it is what it *carries* that stops here.
///
/// Reserved whether the bar is there or not. It only shows when there is
/// something to scroll, so a padding that came with it would slide every row's
/// contents sideways the day a list grows one line too long — and back again
/// when a file is staged.
///
/// The width is the component's own, not a number of ours: the bar is theirs.
pub fn scroll_gutter() -> Pixels {
    gpui_component::scroll::Scrollbar::width()
}

/// The height of a header bar: tabs, panel titles.
pub fn bar_height(cx: &App) -> Pixels {
    scaled(cx, 2.2, px(26.))
}

/// The height of the main toolbar, which carries buttons.
pub fn toolbar_height(cx: &App) -> Pixels {
    scaled(cx, 2.7, px(32.))
}

/// The colour of the gutter between panels.
///
/// It is the tab bar's, which `theme::apply` derives from the background by a
/// few percent less lightness: the gutter and the bar are the **same** plane,
/// the one the cards sit on, and painting them two close-but-not-equal colours
/// is exactly what makes a window look badly assembled.
///
/// Deriving it rather than adding it to the twelve bundled palettes is also
/// what gives a theme written by someone else the benefit without knowing it.
pub fn gutter(cx: &App) -> Hsla {
    gpui_component::ActiveTheme::theme(cx).tab_bar
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

/// Colours specific to the review.
///
/// The green and the red are GitHub's, deliberately: they are the hues a
/// reviewer already has in their eye, and the backgrounds are pale enough for
/// the text to stay readable in both modes.
/// The width of one indentation level, and of the rule marking it.
pub const INDENT: f32 = 12.;

/// The vertical rules of the parent levels of a tree row.
///
/// It is what makes a deep tree readable: without them, at six levels of
/// indentation — the common case on a Laravel project — nothing says any more
/// which folder a row belongs to. Both trees of the window use them, the
/// project explorer's and the review's: they are the same gesture, and a list
/// that indents without ruling reads as a list of files whose names happen to
/// start further right.
///
/// The colour is taken rather than read from the theme: the explorer's rows
/// are rendered by the dozen in a virtualised list's closure, where `cx.theme()`
/// borrows the context — it reads it once per frame and hands it over.
pub fn indent_guides(depth: usize, guide: Hsla) -> impl IntoIterator<Item = gpui::Div> + use<> {
    (0..depth).map(move |_| {
        gpui::div()
            .w(px(INDENT))
            .h_full()
            .flex_none()
            .border_l_1()
            .border_color(guide)
    })
}

/// The colour of those rules: pale enough to read as a texture and not as a
/// separator, they are on screen by the dozen.
pub fn indent_guide(cx: &App) -> Hsla {
    gpui_component::ActiveTheme::theme(cx).border.opacity(0.7)
}

/// The place of the chevron a leaf does not have.
///
/// Without it, a file's name and a folder's do not line up, and the indentation
/// says nothing.
pub fn chevron_space() -> gpui::Div {
    gpui::div().w(px(14.)).flex_none()
}

/// The `+n` and `−m` of a volume of work, in the diff's colours.
///
/// Two independent children rather than one wrapped element: the callers drop
/// them straight into their row, which already sets the gap, and a wrapper of
/// our own would give them a second one. A zero is left out — `+0` makes the eye
/// read a number where there is none.
pub fn volume(added: usize, removed: usize, colors: &DiffColors) -> Vec<gpui::Div> {
    let mut parts = Vec::new();
    if added > 0 {
        parts.push(
            gpui::div()
                .flex_none()
                .text_xs()
                .text_color(colors.added_fg)
                .child(format!("+{added}")),
        );
    }
    if removed > 0 {
        parts.push(
            gpui::div()
                .flex_none()
                .text_xs()
                .text_color(colors.removed_fg)
                .child(format!("−{removed}")),
        );
    }
    parts
}

pub struct DiffColors {
    pub added_bg: Hsla,
    pub added_fg: Hsla,
    pub removed_bg: Hsla,
    pub removed_fg: Hsla,
    pub hunk_bg: Hsla,
    pub line_number: Hsla,
    /// The empty half of a two-column row: where one version has nothing
    /// opposite the other. A neutral tint and not a transparent background,
    /// otherwise nothing tells "no line here" from an empty line.
    pub absent_bg: Hsla,
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
                absent_bg: Hsla {
                    a: 0.05,
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
                absent_bg: Hsla {
                    a: 0.05,
                    ..rgb(0x1f2328)
                },
            }
        }
    }
}

/// The colour of a file's status letter, in the review list.
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

    /// A theme the registry fails to read is simply ignored: no error reaches
    /// the screen, it is just missing from the list. This test is the only
    /// place a typo in a JSON shows.
    #[test]
    fn every_bundled_theme_parses() {
        let mut count = 0;
        for name in BundledThemes::iter() {
            let file = BundledThemes::get(&name).expect("embedded file");
            let text = std::str::from_utf8(&file.data).expect("UTF-8");
            let set: gpui_component::ThemeSet =
                serde_json::from_str(text).unwrap_or_else(|e| panic!("{name} unreadable: {e}"));
            assert!(!set.themes.is_empty(), "{name} declares no theme");
            for theme in &set.themes {
                assert!(!theme.name.is_empty(), "{name}: a theme with no name");
            }
            count += 1;
        }
        assert!(
            count >= 10,
            "the bundled themes have vanished from the binary"
        );
    }

    /// A missing key raises no error: it falls back to the default value, which
    /// is *light*. On a dark theme that makes a white patch in the middle of
    /// the window, and nothing points it out.
    #[test]
    fn no_bundled_theme_leaves_a_colour_unset() {
        let reference: std::collections::BTreeSet<String> = keys_of("claudhub-nord.json");
        for name in BundledThemes::iter() {
            let keys = keys_of(&name);
            let missing: Vec<_> = reference.difference(&keys).collect();
            assert!(missing.is_empty(), "{name}: missing colours {missing:?}");
        }
    }

    /// The interface colours **and** the syntax ones: a theme missing `keyword`
    /// or `embedded` renders code in a single hue, which is just as noticeable
    /// as a white patch.
    fn keys_of(name: &str) -> std::collections::BTreeSet<String> {
        let file = BundledThemes::get(name).expect("embedded file");
        let value: serde_json::Value = serde_json::from_slice(&file.data).expect("JSON");
        let theme = &value["themes"][0];
        let colors = theme["colors"]
            .as_object()
            .expect("a colour table")
            .keys()
            .cloned();
        let syntax = theme["highlight"]["syntax"]
            .as_object()
            .expect("a style table")
            .keys()
            .map(|k| format!("syntax.{k}"));
        colors.chain(syntax).collect()
    }
}
