//! Claudhub — poste de travail de revue de code et de pilotage d'agents de
//! codage en terminal, organisé par worktree git.

// rust-i18n lit les catalogues à la compilation via sa macro procédurale ;
// Cargo, lui, ne saurait pas qu'il faut recompiler quand seule une traduction
// change. Ces deux `include_str!` sont là uniquement pour cette invalidation.
const _: &str = include_str!("../assets/i18n/en.json");
const _: &str = include_str!("../assets/i18n/fr.json");

rust_i18n::i18n!("assets/i18n", fallback = "en");

pub mod agent;
pub mod files;
pub mod git;
pub mod logging;
pub mod runtime;
pub mod sentry;
pub mod terminal;
pub mod ui;
pub mod wt;

/// Évite une allocation par chaîne traduite : les catalogues compilés rendent
/// des `Cow::Borrowed(&'static str)`, et une clé sans interpolation devient un
/// `SharedString` statique. À l'échelle d'une frame qui en rend des centaines,
/// c'est la différence entre gratuit et mesurable.
pub fn i18n_shared(value: std::borrow::Cow<'static, str>) -> gpui::SharedString {
    match value {
        std::borrow::Cow::Borrowed(text) => gpui::SharedString::new_static(text),
        std::borrow::Cow::Owned(text) => gpui::SharedString::from(text),
    }
}

/// Toute chaîne visible par l'utilisateur passe par là. Rend un
/// `SharedString`, jamais un `String`.
#[macro_export]
macro_rules! tr {
    ($key:expr) => { $crate::i18n_shared(rust_i18n::t!($key)) };
    ($key:expr, { $($name:ident : $value:expr),* $(,)? }) => {
        $crate::i18n_shared(rust_i18n::t!($key, $($name = $value),*))
    };
}

fn main() {
    logging::init();
    ui::run();
}
