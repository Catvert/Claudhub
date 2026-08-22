//! Plugins: a panel whose content is a script, reloaded while the window runs.
//!
//! The reason this exists is a count. Sentry, undressed, holds almost nothing
//! that speaks of Sentry: fetch JSON behind a token, remember one setting per
//! repository, paint a master/detail list with a code excerpt, compose a prompt
//! and hand it to the agent. Those are five generic capabilities, and they are
//! exactly what a GitHub issue list, a CI board or a log stream would ask for.
//!
//! The split is the repository's own, pushed one notch: `notes.rs` in front of
//! `notes_view.rs`, `sql_history.rs` in front of `sql_history_view.rs`. A
//! script produces a **tree of data** ([`view::Node`]); `ui::plugin_view`
//! paints it. Nothing here knows gpui.
//!
//! Three layers, and each is where it is for a reason:
//!
//! - [`view`] and [`manifest`] are plain data. No gpui, no Rune.
//! - [`caps`] is what a plugin may do to the outside world. Data too, executed
//!   by a worker — which may be a worker in the WSL server. **That is why it
//!   carries no Rune**: the headless binary must be able to run a plugin's
//!   requests without a scripting engine in it.
//! - [`host`] is the Rune machine, behind the `plugins` feature, which `ui`
//!   turns on. The script runs on the interface's side; only its input and
//!   output cross the wire.

pub mod caps;
pub mod manifest;
pub mod view;

#[cfg(feature = "plugins")]
pub mod host;

#[cfg(feature = "plugins")]
mod loaded;
#[cfg(feature = "plugins")]
pub use loaded::Plugin;
