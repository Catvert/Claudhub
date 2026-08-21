//! Claudhub — poste de travail de revue de code et de pilotage d'agents de
//! codage en terminal, organisé par worktree git.
//!
//! La bibliothèque porte tout ; deux binaires s'y branchent : `claudhub`
//! (l'interface gpui) et `claudhub-server` (les mêmes workers, headless,
//! derrière stdin/stdout — c'est lui qui tourne dans WSL2 quand l'interface
//! est un `.exe` Windows). La feature `ui`, active par défaut, porte tout ce
//! qui touche gpui : le serveur se construit avec `--no-default-features`, et
//! c'est ce qui garantit qu'aucun module du cœur n'en dépend.

// rust-i18n lit les catalogues à la compilation via sa macro procédurale ;
// Cargo, lui, ne saurait pas qu'il faut recompiler quand seule une traduction
// change. Ces deux `include_str!` sont là uniquement pour cette invalidation.
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
pub mod runtime;
pub mod sentry;
#[cfg(feature = "ui")]
pub mod terminal;
#[cfg(feature = "ui")]
pub mod ui;
pub mod wslpath;
pub mod wt;

/// Évite une allocation par chaîne traduite : les catalogues compilés rendent
/// des `Cow::Borrowed(&'static str)`, et une clé sans interpolation devient un
/// `SharedString` statique. À l'échelle d'une frame qui en rend des centaines,
/// c'est la différence entre gratuit et mesurable.
#[cfg(feature = "ui")]
pub fn i18n_shared(value: std::borrow::Cow<'static, str>) -> gpui::SharedString {
    match value {
        std::borrow::Cow::Borrowed(text) => gpui::SharedString::new_static(text),
        std::borrow::Cow::Owned(text) => gpui::SharedString::from(text),
    }
}

/// Toute chaîne visible par l'utilisateur passe par là. Rend un
/// `SharedString`, jamais un `String`. Le macro n'existe que sous la feature
/// `ui` : un module du cœur qui s'en servirait casserait la compilation du
/// serveur, et c'est le garde voulu.
#[cfg(feature = "ui")]
#[macro_export]
macro_rules! tr {
    ($key:expr) => { $crate::i18n_shared(rust_i18n::t!($key)) };
    ($key:expr, { $($name:ident : $value:expr),* $(,)? }) => {
        $crate::i18n_shared(rust_i18n::t!($key, $($name = $value),*))
    };
}
