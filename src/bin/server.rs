//! `claudhub-server` — Claudhub's workers, headless.
//!
//! This is the binary that runs inside the WSL2 distribution when the
//! interface is a Windows `.exe`: same queues, same `runtime::handle`, but the
//! `Cmd`s arrive on standard input and the `Evt`s leave on standard output —
//! **stdout belongs to the wire**, traces go to stderr (see `runtime::wire`).
//! It builds without the `ui` feature, which is the gate proving no core
//! module pulls in gpui.
//!
//! Its lifetime is the parent's: the end of standard input is the order to
//! shut down, and losing standard output (the parent died without closing
//! cleanly) is one too.

use claudhub::runtime::{self, wire};

fn main() {
    // Before anything else, and before any thread exists: this server is what
    // will launch the agents (through wt and commit_msg), and the session
    // markers of whatever launched us would make each of them a sub-session of
    // its own.
    claudhub::agent::disinherit_session();
    claudhub::logging::init();

    let mut stdout = std::io::stdout().lock();
    // The stdin lock is not `Send`: we keep the handle and lock inside the
    // thread that reads.
    let stdin = std::io::stdin();

    // Our handshake goes first; the client's follows. Both ends write before
    // reading: two small frames fit in the pipe buffers, so neither waits on
    // the other. One that cannot be written is the end of the server, said on
    // stderr — which is what the client quotes in its `ServerLost`.
    if let Err(e) = wire::write_hello(&mut stdout, &wire::Hello::current()) {
        eprintln!("claudhub-server: {e}");
        std::process::exit(1);
    }
    let client: wire::Hello = match wire::read_frame(&mut stdin.lock()) {
        Ok(Some(hello)) => hello,
        Ok(None) => std::process::exit(0), // gone before introducing itself
        Err(e) => {
            eprintln!("claudhub-server: unreadable handshake: {e}");
            std::process::exit(1);
        }
    };
    // The client makes the same check on its side; this one catches a client
    // too old to know how.
    if client.protocol != wire::PROTOCOL_VERSION {
        eprintln!(
            "claudhub-server: version mismatch (client {}, server {})",
            client.protocol,
            wire::PROTOCOL_VERSION
        );
        std::process::exit(3);
    }

    let (handle, evt_rx) = runtime::spawn();

    // Input: one frame, one `Cmd`, the same delivery as the view's — it is
    // `Handle::send` that sorts them back into the queues.
    std::thread::Builder::new()
        .name("claudhub-server-in".into())
        .spawn(move || loop {
            // The ticket the window issued goes on with the command: it is
            // the one the answer must carry.
            match wire::read_frame::<runtime::Order>(&mut stdin.lock()) {
                Ok(Some(order)) => handle.submit(order),
                // Clean end: the parent closed the wire, we follow.
                Ok(None) => std::process::exit(0),
                Err(e) => {
                    eprintln!("claudhub-server: unreadable input stream: {e}");
                    std::process::exit(1);
                }
            }
        })
        .expect("input thread");

    // Output, on the main thread: events from every queue, serialised in the
    // order they leave the shared channel.
    while let Ok(evt) = evt_rx.recv_blocking() {
        if let Err(e) = wire::write_frame(&mut stdout, &evt) {
            eprintln!("claudhub-server: the parent stopped listening: {e}");
            std::process::exit(1);
        }
    }
}
