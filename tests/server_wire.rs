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

#[test]
fn an_answer_names_the_command_it_answers() {
    // The ticket `send` hands back crosses the wire with its command, and
    // comes back on the answer: a write among several of the same kind is
    // told by nothing else.
    let argv = vec![env!("CARGO_BIN_EXE_claudhub-server").to_string()];
    let (handle, events) = connect(&argv).expect("launching the server");
    assert!(matches!(next(&events), Evt::ServerHello { .. }));

    let nowhere = PathBuf::from("/claudhub-nowhere");
    let first = handle.send(Cmd::RefreshRepo {
        main: nowhere.clone(),
    });
    let second = handle.send(Cmd::RefreshRepo { main: nowhere });
    assert_ne!(first, second);
    let mut answered = Vec::new();
    while answered.len() < 2 {
        if let Evt::Failed { ticket, .. } = next(&events) {
            answered.push(ticket);
        }
    }
    answered.sort_by_key(|ticket| ticket.0);
    assert_eq!(answered, vec![first, second]);
    drop(handle);
}

#[test]
fn a_server_that_dies_early_says_why() {
    // What it wrote on stderr before going is the reason, and it belongs in
    // the event the window shows — not only in the journal.
    let script = "echo 'there is no distribution with the supplied name' >&2; exit 1";
    let argv = ["sh", "-c", script].map(String::from);
    let (_handle, events) = connect(&argv).expect("launch");
    match next(&events) {
        Evt::ServerLost { message } => {
            assert!(message.contains("no distribution"), "{message}")
        }
        other => panic!("expected ServerLost, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn a_launch_directory_the_wire_cannot_carry_is_a_lost_server_not_a_hang() {
    // The server's handshake carries its launch directory, and a name that is
    // not UTF-8 does not serialise. Dropped in silence, as any other frame
    // would be, it left the window waiting for a hello and the server for the
    // window's, for ever.
    let root = std::env::temp_dir().join(format!("claudhub-wire-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("temp dir");
    let script = r#"d="$0/$(printf '\377')"; mkdir -p "$d" && cd "$d" && exec "$1""#;
    let argv = [
        "sh".to_string(),
        "-c".to_string(),
        script.to_string(),
        root.display().to_string(),
        env!("CARGO_BIN_EXE_claudhub-server").to_string(),
    ];
    let (_handle, events) = connect(&argv).expect("launch");
    let evt = next(&events);
    let _ = std::fs::remove_dir_all(&root);
    match evt {
        Evt::ServerLost { message } => assert!(message.contains("not UTF-8"), "{message}"),
        other => panic!("expected ServerLost, got {other:?}"),
    }
}
