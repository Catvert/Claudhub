//! One window per machine, and the folder later launches hand to it.
//!
//! The Explorer's "Open with Claudhub" appears on every folder, and a second
//! click used to mean a second process: another window, another WSL server,
//! another set of watchers on the same worktrees, and two sidebars that each
//! remember half of what one opened. What the gesture means is "show me this
//! repository", and that is a message to the window already up.
//!
//! **The transport is a local socket** — a named pipe on Windows
//! (`\\.\pipe\…`), an abstract socket on Linux. `GenericNamespaced` because
//! neither leaves a file behind: a name released when the process dies is a
//! name with no stale-lock story, and a workstation that will not start
//! because it was killed once is a worse bug than the one this fixes.
//!
//! **Nothing here may keep the window from opening.** Every failure — no
//! socket, no permission, a peer that dies mid-sentence — ends in "you are the
//! only instance", which is what the program did before this module existed.
//! `CLAUDHUB_ALLOW_MULTIPLE` says so on purpose, for the second window one
//! wants while developing.
//!
//! **The name carries the user.** Both namespaces are machine-wide, so without
//! it the second person to log in would hand their folders to the first one's
//! window — or, on Windows, fail to create the pipe at all.
//!
//! **What arrives is untrusted.** The Windows pipe is created with the default
//! security descriptor, so its own user and the administrators; a Linux
//! abstract socket, on the other hand, has **no permission check at all** —
//! any process on the machine can connect. The payload is therefore read as
//! UTF-8 and refused otherwise, never handed to
//! `OsStr::from_encoded_bytes_unchecked`, whose contract it would not meet.
//! That leaves a local program able to make this window come forward and open
//! a folder, which is what the gesture does anyway; it buys nothing else.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use interprocess::local_socket::{
    prelude::*, GenericNamespaced, ListenerOptions, Stream as LocalStream,
};

/// What a launch turned out to be.
pub enum Instance {
    /// The only one. Folders named by later launches arrive here; `None` is a
    /// launch with no folder, which asks for nothing but the window.
    Only(async_channel::Receiver<Option<PathBuf>>),
    /// Another window is up, and has been given the folder. Nothing left to do
    /// but leave — quietly: the user clicked a menu, they did not ask for a
    /// report.
    Handed,
}

/// Claims the machine's single instance, handing `folder` over if one is
/// already running.
pub fn claim(folder: Option<&Path>) -> Instance {
    if std::env::var_os("CLAUDHUB_ALLOW_MULTIPLE").is_some() {
        log::info!("CLAUDHUB_ALLOW_MULTIPLE is set: not claiming the single instance");
        return Instance::Only(inert());
    }
    claim_named(&default_name(), folder)
}

/// The folder named on the command line, resolved against the current
/// directory.
///
/// Read once, in `main`, and used twice: to hand over, and — when we turn out
/// to be the only instance — to decide which repository opens.
pub fn folder_named() -> Option<PathBuf> {
    launch_argument(
        &std::env::args_os().skip(1).collect::<Vec<_>>(),
        std::env::current_dir().ok().as_deref(),
    )
}

/// The socket name: one per user, since both namespaces are machine-wide.
///
/// A name that is not a plain word is refused by `GenericNamespaced` on
/// Windows, where it becomes a path element; the user name comes from the
/// environment and is therefore not ours to trust.
fn default_name() -> String {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();
    socket_name(&user)
}

/// The name that user gets. Split out from the environment so it can be tested
/// without writing to it: `setenv` in a process running tests on several
/// threads races with every `getenv` beside it.
fn socket_name(user: &str) -> String {
    let user: String = user
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(32)
        .collect();
    format!(
        "claudhub-{}.sock",
        if user.is_empty() { "user" } else { &user }
    )
}

/// The same, on a name of one's choosing — what the test uses, so that running
/// it does not talk to the Claudhub open on the same machine.
fn claim_named(name: &str, folder: Option<&Path>) -> Instance {
    // Connect first, create second, and twice around: whoever answers owns the
    // window, and the only way to lose this race is for two launches to find
    // the name free at the same instant. The loser of *that* finds it taken on
    // the second pass.
    for _ in 0..2 {
        if hand_over(name, folder) {
            return Instance::Handed;
        }
        match listen(name) {
            Ok(listener) => return Instance::Only(accept_in_a_thread(listener)),
            // Somebody created it between our two calls: go and knock.
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => continue,
            Err(e) => {
                log::warn!("no single-instance socket ({e}): this window stands alone");
                return Instance::Only(inert());
            }
        }
    }
    log::warn!("the single-instance socket kept changing hands: this window stands alone");
    Instance::Only(inert())
}

/// Hands the folder to the running instance, and says whether there was one.
///
/// The payload is the path and nothing else: one connection is one message, so
/// end of stream is the end of it and there is no framing to get wrong. An
/// empty payload is a launch with no folder — "come forward", which is also
/// what a path this side cannot spell in UTF-8 comes down to. That is the
/// wire's own limit too: postcard carries a `PathBuf` as a string, so a folder
/// we could not spell is one the server could not be told about either.
fn hand_over(name: &str, folder: Option<&Path>) -> bool {
    let Ok(name) = name.to_ns_name::<GenericNamespaced>() else {
        return false;
    };
    let Ok(mut stream) = LocalStream::connect(name) else {
        return false; // nobody there, which is the ordinary case
    };
    // Windows only lets the foreground process, or one it spawned, raise a
    // window; we are the one Explorer just started, and the window to raise
    // belongs to somebody else. This is the hand-off of that right, and
    // without it `SetForegroundWindow` over there merely flashes a taskbar
    // button — the click would look like it did nothing.
    allow_foreground();
    let payload = folder.and_then(|p| p.to_str()).unwrap_or("");
    if let Err(e) = stream.write_all(payload.as_bytes()) {
        // It answered and then died: it was there, and starting a second
        // window now would be the surprise this module exists to avoid.
        log::warn!("the running instance did not take the folder: {e}");
    }
    true
}

fn listen(name: &str) -> std::io::Result<interprocess::local_socket::Listener> {
    let name = name.to_ns_name::<GenericNamespaced>()?;
    ListenerOptions::new().name(name).create_sync()
}

/// Reads the folders handed over, one connection at a time.
///
/// A thread of its own, blocking on `accept`: the alternative is a poll in the
/// frame loop for something that happens twice a day.
fn accept_in_a_thread(
    listener: interprocess::local_socket::Listener,
) -> async_channel::Receiver<Option<PathBuf>> {
    let (tx, rx) = async_channel::unbounded();
    let spawned = std::thread::Builder::new()
        .name("claudhub-instance".into())
        .spawn(move || {
            for conn in listener.incoming() {
                let mut conn = match conn {
                    Ok(conn) => conn,
                    // One refused connection is not the end of the listener:
                    // the next launch deserves its chance.
                    Err(e) => {
                        log::warn!("a launch could not be heard out: {e}");
                        continue;
                    }
                };
                let mut payload = Vec::new();
                if let Err(e) = conn.read_to_end(&mut payload) {
                    log::warn!("a launch was cut short: {e}");
                    continue;
                }
                // The window is gone: so is the reason to listen.
                if tx.send_blocking(decode(&payload)).is_err() {
                    return;
                }
            }
        });
    if let Err(e) = spawned {
        log::warn!("no thread to hear later launches ({e}): this window stands alone");
        return inert();
    }
    rx
}

/// The payload back into a path. Empty means "no folder, just the window".
///
/// Checked, and not `OsStr::from_encoded_bytes_unchecked`: on Linux anyone can
/// reach this socket, and that function's contract — bytes this very standard
/// library produced from an `OsStr` — is not something a caller can promise on
/// somebody else's behalf.
fn decode(payload: &[u8]) -> Option<PathBuf> {
    if payload.is_empty() {
        return None;
    }
    match std::str::from_utf8(payload) {
        Ok(path) => Some(PathBuf::from(path)),
        Err(_) => {
            log::warn!("a launch named a folder that is not UTF-8: only the window comes forward");
            None
        }
    }
}

/// A receiver nothing will ever send to: the shape of "you are alone, and no
/// one can reach you". The sender is dropped, so the window's task ends at
/// once instead of waiting for ever.
fn inert() -> async_channel::Receiver<Option<PathBuf>> {
    let (_tx, rx) = async_channel::unbounded();
    rx
}

#[cfg(windows)]
fn allow_foreground() {
    // ASFW_ANY: we do not know the other window's process, and learning it
    // would mean a round trip on a socket we are about to close. The right is
    // spent by the first `SetForegroundWindow` that follows, which is the one
    // we came here for.
    const ASFW_ANY: u32 = u32::MAX;
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(ASFW_ANY);
    }
}

#[cfg(not(windows))]
fn allow_foreground() {}

/// The directory named on the command line, resolved against the launch
/// directory.
///
/// `claudhub ~/projects/thing` opens that repository, which is what a shell
/// means by it — and, on Windows, it is how the Explorer's "Open with
/// Claudhub" says which folder was right-clicked: a shell verb's working
/// directory is whatever the Explorer happened to have, so the path has to
/// travel as an argument.
///
/// The first argument that is not an option, and no parsing beyond that: there
/// is nothing else on this command line, and an unknown `--flag` must not be
/// taken for a folder.
///
/// `Path::join` and not `wslpath::join` here, deliberately: both sides are the
/// **host's** — this runs before anything reaches the wire, and a relative path
/// is relative to the machine that typed it.
fn launch_argument(args: &[std::ffi::OsString], cwd: Option<&Path>) -> Option<PathBuf> {
    let named = args
        .iter()
        .find(|arg| !arg.to_string_lossy().starts_with('-'))
        .map(PathBuf::from)?;
    Some(match cwd {
        Some(cwd) if named.is_relative() => cwd.join(named),
        _ => named,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<std::ffi::OsString> {
        list.iter().map(std::ffi::OsString::from).collect()
    }

    #[test]
    fn an_absolute_argument_is_the_launch_directory() {
        assert_eq!(
            launch_argument(&args(&["/r/wt/a"]), Some(Path::new("/elsewhere"))),
            Some(PathBuf::from("/r/wt/a"))
        );
    }

    #[test]
    fn a_relative_argument_is_resolved_where_it_was_typed() {
        assert_eq!(
            launch_argument(&args(&["wt/a"]), Some(Path::new("/r"))),
            Some(PathBuf::from("/r/wt/a"))
        );
    }

    /// What the Explorer's verb passes is a folder; what a shell may pass in
    /// front of it is an option, and taking `--flag` for a folder would open a
    /// repository named after it.
    #[test]
    fn an_option_is_not_a_folder() {
        assert_eq!(
            launch_argument(&args(&["--flag"]), Some(Path::new("/r"))),
            None
        );
        assert_eq!(
            launch_argument(&args(&["--flag", "/r/wt/a"]), Some(Path::new("/r"))),
            Some(PathBuf::from("/r/wt/a"))
        );
    }

    #[test]
    fn no_argument_names_nothing() {
        assert_eq!(launch_argument(&[], Some(Path::new("/r"))), None);
    }

    #[test]
    fn the_user_name_cannot_shape_the_socket_name() {
        // It comes from the environment: a name that is not a plain word is
        // refused by the Windows namespace, where it becomes a path element.
        assert_eq!(socket_name("../../etc/passwd"), "claudhub-etcpasswd.sock");
        assert_eq!(socket_name(""), "claudhub-user.sock");
        assert_eq!(socket_name("finch"), "claudhub-finch.sock");
    }

    #[test]
    fn a_payload_that_is_not_utf8_names_no_folder() {
        // Anyone can reach the Linux socket: what comes out of it is checked,
        // never trusted into `from_encoded_bytes_unchecked`.
        assert_eq!(decode(&[0xff, 0xfe]), None);
        assert_eq!(decode(b""), None);
        assert_eq!(decode(b"/r/wt/a"), Some(PathBuf::from("/r/wt/a")));
    }

    /// The whole chain, with the real socket: a first launch claims the name,
    /// a second one hands it a folder and is told to leave, and the folder
    /// comes out of the receiver.
    ///
    /// The counterpart of `watch::tests::a_real_write_reaches_the_receiver`,
    /// and for the same reason: what breaks here breaks between two processes,
    /// where no unit test would be looking.
    #[test]
    fn a_second_launch_hands_its_folder_over_and_leaves() {
        // Its own name: the machine running this test may well have Claudhub
        // open, and the point is not to talk to it.
        let name = format!("claudhub-test-{}.sock", std::process::id());

        let Instance::Only(handoffs) = claim_named(&name, None) else {
            panic!("the first launch is the only instance");
        };

        let folder = PathBuf::from("/r/wt/a");
        let Instance::Handed = claim_named(&name, Some(&folder)) else {
            panic!("the second launch hands over and leaves");
        };
        assert_eq!(handoffs.recv_blocking().unwrap(), Some(folder));

        // And a launch with nothing to say still asks for the window.
        let Instance::Handed = claim_named(&name, None) else {
            panic!("a launch with no folder hands over too");
        };
        assert_eq!(handoffs.recv_blocking().unwrap(), None);
    }
}
