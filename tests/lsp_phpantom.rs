//! The client against a real language server.
//!
//! The unit tests drive the session loop with no process at all, which proves
//! the protocol and proves nothing about the pipes: the framing on a real
//! stdout, a server that answers `initialize` in its own time, a diagnostic
//! that arrives unasked. This is the same reasoning as `server_wire.rs`, which
//! runs the real `claudhub-server` rather than a mock of it.
//!
//! **Skipped when `phpantom_lsp` is not installed**, which is the case on CI:
//! a test that fails for want of a program nobody promised is a red build that
//! says nothing. Locally, it is the only thing that proves the chain end to
//! end.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use claudhub::lsp::{Host, Server};
use claudhub::runtime::protocol::Evt;

/// PHPantom reads a Composer classmap before it answers: generous, and it
/// normally lands in a second or two.
const READY: Duration = Duration::from_secs(60);

fn installed() -> bool {
    std::process::Command::new("phpantom_lsp")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// A project on disk, because a language server's whole job is to read one.
fn project() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("claudhub-lsp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("composer.json"),
        r#"{"autoload": {"psr-4": {"App\\": "src/"}}}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("src/User.php"),
        "<?php\nnamespace App;\nclass User {\n    public function name(): string { return 'x'; }\n}\n",
    )
    .unwrap();
    dir
}

fn wait<T>(
    events: &async_channel::Receiver<Evt>,
    deadline: Duration,
    mut want: impl FnMut(&Evt) -> Option<T>,
) -> Option<T> {
    let until = Instant::now() + deadline;
    while Instant::now() < until {
        match events.try_recv() {
            Ok(event) => {
                if let Some(found) = want(&event) {
                    return Some(found);
                }
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    None
}

#[test]
fn a_real_server_starts_opens_a_file_and_answers() {
    if !installed() {
        eprintln!("phpantom_lsp is not installed: skipped");
        return;
    }
    let worktree = project();
    let (events_tx, events_rx) = async_channel::unbounded();
    let host = Host::new(events_tx);
    let server = Server {
        name: "PHPantom".into(),
        command: "phpantom_lsp".into(),
        extensions: vec!["php".into()],
        language_id: "php".into(),
        ..Default::default()
    };
    host.start(worktree.clone(), server);

    let capabilities = wait(&events_rx, READY, |event| match event {
        Evt::LspReady { capabilities, .. } => Some(capabilities.clone()),
        Evt::LspStopped { reason, .. } => panic!("the server stopped: {reason:?}"),
        _ => None,
    })
    .expect("the handshake never came back");
    // What the editor's providers are posted from: a server that says none of
    // this would leave the editor with nothing to ask.
    assert!(capabilities.contains("completionProvider"));
    assert!(capabilities.contains("hoverProvider"));
    assert!(capabilities.contains("definitionProvider"));

    let path = worktree.join("src/User.php");
    host.ask(
        &worktree,
        claudhub::lsp::Ask::Open {
            path: path.clone(),
            language_id: "php".into(),
            text: std::fs::read_to_string(&path).unwrap(),
        },
    );
    host.ask(
        &worktree,
        claudhub::lsp::Ask::Request {
            id: 42,
            method: "textDocument/hover".into(),
            // On `User`, in the class declaration.
            params: serde_json::json!({
                "textDocument": {"uri": claudhub::lsp::uri::of(&path)},
                "position": {"line": 2, "character": 7},
            })
            .to_string(),
        },
    );
    let answer = wait(&events_rx, READY, |event| match event {
        Evt::LspAnswer { id: 42, result, .. } => Some(result.clone()),
        _ => None,
    })
    .expect("no answer to the hover");
    assert!(answer.is_ok(), "{answer:?}");

    // The other half of the feature, and the half nobody asks for: diagnostics
    // arrive unasked for a file that was merely opened.
    //
    // That they arrive is ours; **what is in them is not**. Which mistakes a
    // server reports is its business and its version's — PHPantom's undefined
    // variable rules have moved between releases — and asserting on the text
    // would make this test fail the day they tune a rule, which says nothing
    // about the client.
    let broken = worktree.join("src/Broken.php");
    std::fs::write(
        &broken,
        "<?php\nnamespace App;\nclass Broken {\n    public function go(): int { return $nope; }\n}\n",
    )
    .unwrap();
    host.ask(
        &worktree,
        claudhub::lsp::Ask::Open {
            path: broken.clone(),
            language_id: "php".into(),
            text: std::fs::read_to_string(&broken).unwrap(),
        },
    );
    wait(&events_rx, READY, |event| match event {
        Evt::LspDiagnostics { path, .. } if *path == broken => Some(()),
        _ => None,
    })
    .expect("no diagnostics for the file that was opened");

    host.stop(worktree.clone());
    let _ = std::fs::remove_dir_all(&worktree);
}
