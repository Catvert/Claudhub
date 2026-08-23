//! The interface binary: everything lives in the `claudhub` library.
//!
//! **A window, and no console under it.** A Rust executable is a console
//! program by default, so Windows opened a black window beside the interface
//! and poured `env_logger` into it — a second window nobody asked for, which
//! closing kills the application. The subsystem is therefore declared, and what
//! that console showed is what the "Journal" page of the settings exists for.
//! Its price is that stderr goes nowhere on Windows: a GUI program has no
//! console to inherit, which is also why every console child is launched
//! through `wsl::no_console`.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    // Before anything else, and before any thread exists: what launched us is
    // often an agent, and its session markers would make every agent we start
    // a sub-session of its own.
    claudhub::agent::disinherit_session();
    claudhub::logging::init();
    claudhub::ui::run();
}
