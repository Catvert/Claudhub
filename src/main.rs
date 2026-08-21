//! The interface binary: everything lives in the `claudhub` library.

fn main() {
    // Before anything else, and before any thread exists: what launched us is
    // often an agent, and its session markers would make every agent we start
    // a sub-session of its own.
    claudhub::agent::disinherit_session();
    claudhub::logging::init();
    claudhub::ui::run();
}
