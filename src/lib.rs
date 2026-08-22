//! Claudhub — a workstation for code review and for driving terminal coding
//! agents, organised by git worktree.
//!
//! The library carries everything; two binaries plug into it: `claudhub` (the
//! gpui interface) and `claudhub-server` (the same workers, headless, behind
//! stdin/stdout — the one that runs inside WSL2 when the interface is a
//! Windows `.exe`). The `ui` feature, on by default, carries everything that
//! touches gpui: the server builds with `--no-default-features`, and that is
//! what guarantees no core module depends on it.

// rust-i18n reads the catalogues at compile time through its proc macro;
// Cargo, for its part, would not know to rebuild when only a translation
// changes. These two `include_str!` exist solely for that invalidation.
#[cfg(feature = "ui")]
const _: &str = include_str!("../assets/i18n/en.json");
#[cfg(feature = "ui")]
const _: &str = include_str!("../assets/i18n/fr.json");

#[cfg(feature = "ui")]
rust_i18n::i18n!("assets/i18n", fallback = "en");

pub mod agent;
pub mod cmdline;
pub mod commit_msg;
pub mod db;
pub mod files;
pub mod git;
pub mod logging;
pub mod lsp;
pub mod plugin;
pub mod runtime;
#[cfg(feature = "ui")]
pub mod terminal;
#[cfg(feature = "ui")]
pub mod ui;
pub mod wsl;
pub mod wslpath;
pub mod wt;

/// Avoids one allocation per translated string: compiled catalogues yield
/// `Cow::Borrowed(&'static str)`, and a key without interpolation becomes a
/// static `SharedString`. Across a frame that renders hundreds of them, that is
/// the difference between free and measurable.
#[cfg(feature = "ui")]
pub fn i18n_shared(value: std::borrow::Cow<'static, str>) -> gpui::SharedString {
    match value {
        std::borrow::Cow::Borrowed(text) => gpui::SharedString::new_static(text),
        std::borrow::Cow::Owned(text) => gpui::SharedString::from(text),
    }
}

/// Every user-visible string goes through this. Yields a `SharedString`, never
/// a `String`. The macro only exists under the `ui` feature: a core module
/// using it would break the server build, and that is the intended guard.
#[cfg(feature = "ui")]
#[macro_export]
macro_rules! tr {
    ($key:expr) => { $crate::i18n_shared(rust_i18n::t!($key)) };
    ($key:expr, { $($name:ident : $value:expr),* $(,)? }) => {
        $crate::i18n_shared(rust_i18n::t!($key, $($name = $value),*))
    };
}
