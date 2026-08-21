//! The whole remote transport chain, on this machine.
//!
//! This is the gate for server mode: the real `claudhub-server` binary (cargo
//! builds it for this test), the real handshake, a `Cmd` going down and the
//! `Evt` coming back. The window is not here, but everything between it and
//! the server is — `CLAUDHUB_SERVER_CMD` wires exactly this path into the
//! application.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use claudhub::runtime::remote::connect;
use claudhub::runtime::{Cmd, Evt};

/// The next event, with a test's impatience: the channel is drained by a gpui
/// pump in the application, here by a short loop.
fn next(events: &async_channel::Receiver<Evt>) -> Evt {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        match events.try_recv() {
            Ok(evt) => return evt,
            Err(async_channel::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(async_channel::TryRecvError::Closed) => panic!("the wire closed"),
        }
    }
    panic!("no event from the server in thirty seconds");
}

#[test]
fn the_server_answers_over_the_wire() {
    let argv = vec![env!("CARGO_BIN_EXE_claudhub-server").to_string()];
    let (handle, events) = connect(&argv).expect("launching the server");

    // The handshake first, always: versions agreed, plus what the server knows
    // about its own machine.
    match next(&events) {
        Evt::ServerHello { cwd, .. } => assert!(cwd.is_absolute(), "{cwd:?}"),
        other => panic!("expected ServerHello, got {other:?}"),
    }

    // An order that crosses everything: the frame, the server's queues, the
    // git worker, and the answer — a repository that does not exist answers
    // cleanly.
    let nowhere = PathBuf::from("/claudhub-nowhere");
    handle.send(Cmd::OpenRepo(nowhere.clone()));
    loop {
        match next(&events) {
            Evt::RepoUnavailable { path, message } => {
                assert_eq!(path, nowhere);
                assert!(!message.is_empty());
                break;
            }
            // The server's watcher may speak in the meantime; this is not
            // about it.
            _ => continue,
        }
    }
    // Dropping the handle closes the server's input, and it shuts down: that
    // is the nominal lifetime, and a server that survived would leave one
    // orphan process per test.
    drop(handle);
}

#[test]
fn a_dead_server_is_reported_not_swallowed() {
    // A "server" that dies before introducing itself: this is also what the
    // view sees when the real one is killed mid-flight, and the answer must be
    // an event it can display, never silence.
    let (_handle, events) = connect(&["true".to_string()]).expect("launch");
    match next(&events) {
        Evt::ServerLost { message } => assert!(!message.is_empty()),
        other => panic!("expected ServerLost, got {other:?}"),
    }
}
