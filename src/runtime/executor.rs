//! The shared async executor.
//!
//! Claudhub stays a threaded program: the workers in `runtime::mod` consume
//! `Cmd`s and spawn git subprocesses, because a `fork` blocks anyway and there
//! is nothing to interleave between the call and the result. This module
//! changes none of that — it **adds** a tokio executor alongside, for the
//! libraries that have no blocking interface.
//!
//! The first is `sqlx`, whose driver is async throughout. What it brings that a
//! blocking driver could not:
//!
//! - **A timeout that really cancels.** `tokio::time::timeout` abandons the
//!   running query by dropping its future. A blocking driver has nothing to
//!   cancel: it has to be talked into stopping by some means of its own — a
//!   progress callback for SQLite, a socket timeout for MySQL — and whatever
//!   the driver did not plan for does not get interrupted at all.
//! - **One stack for what comes next.** An async HTTP client for Sentry, git
//!   subprocesses launched side by side (`tokio::process`), a file watcher:
//!   anything wanting async will find the executor here rather than bringing a
//!   second one along.
//!
//! **The bridge is `block_on`, and it lives in exactly one place** — the worker
//! handling the command. That is what keeps `runtime::handle` synchronous and
//! pure: it returns a `Vec<Evt>`, it knows nothing of the channel, and it can
//! be tested. A worker awaiting a future waits exactly as it waited for `git`.
//!
//! **Never from the interface thread.** `block_on` would freeze the window
//! there, which is precisely what the `Cmd`/`Evt` protocol exists to avoid;
//! gpui has its own executor for what the view needs.

use std::sync::OnceLock;

use tokio::runtime::Runtime;

/// Executor threads.
///
/// Two, and not the core count tokio takes by default: what runs on them is
/// waiting on a socket or a file, there is no computation to spread, and a
/// sixteen-core machine has no reason to carry sixteen sleeping threads. It
/// also bounds concurrency towards a server we would rather not flood.
const WORKERS: usize = 2;

fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKERS)
            .thread_name("claudhub-async")
            // `enable_all`: time (timeouts) and network (sockets). Without it a
            // `timeout` panics at run time announcing there is no timer — and
            // only once it is reached.
            .enable_all()
            .build()
            .expect("the system refuses to create the async executor")
    })
}

/// Awaits a future from a worker thread.
///
/// The executor starts on first call: a window that never opens a database
/// does not pay for its threads.
pub fn block_on<F: std::future::Future>(future: F) -> F::Output {
    runtime().block_on(future)
}

/// A way to spawn a task on the shared executor.
///
/// Nothing uses it yet: it is the entry point for whatever wants to work side
/// by side — several queries, an HTTP client — without going back through a
/// worker that waits.
#[allow(dead_code)]
pub fn handle() -> tokio::runtime::Handle {
    runtime().handle().clone()
}
