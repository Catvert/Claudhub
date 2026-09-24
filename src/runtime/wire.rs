//! The wire between the interface and `claudhub-server`: frames and format.
//!
//! A frame is a four-byte little-endian length followed by a postcard body.
//! `Cmd`s go down the server's standard input, `Evt`s come back up its
//! standard output — **stdout belongs to the wire**: a `println!` in worker
//! code would corrupt it, traces go to stderr.
//!
//! Secrets travel in the clear: the wire is a private pipe to a child process
//! of the same session, not a network. It is the same level of trust as an
//! environment variable.
//!
//! A non-UTF-8 path does not serialise (it is `PathBuf` that refuses, whatever
//! the format): sending logs it and drops it rather than closing the wire —
//! see [`write_frame`]. Except in the handshake, where a dropped frame is two
//! ends waiting on each other for ever: see [`write_hello`].

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;

/// The protocol version. To be incremented on **every** change to `Cmd`, to
/// `Evt` or to a type they carry: the two ends are two binaries shipped
/// together but installed separately, and a disagreement should be told at the
/// handshake rather than as an unreadable frame on the first diff.
pub const PROTOCOL_VERSION: u32 = 50;

/// The first frame from each end, before any `Cmd` or `Evt`.
///
/// It is **outside** the two big enums, and its fields are never reordered: it
/// is what detects a version mismatch, so it has to be readable from any
/// version — postcard is positional, and an enum that has moved decodes
/// gibberish without saying so.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Hello {
    pub protocol: u32,
    /// The build id (`CLAUDHUB_BUILD_ID` in CI, `dev` elsewhere): for traces,
    /// the authoritative version is `protocol`.
    pub build: String,
    /// The server's launch directory: it replaces the view's `current_dir`
    /// when the workers run elsewhere.
    pub cwd: PathBuf,
    /// True if the server runs under WSL — that is where "this path is a
    /// Windows mount" means something, and the view cannot guess it.
    pub running_under_wsl: bool,
    /// The server's login shells (`/etc/shells`): the list the settings form
    /// offers, and which a Windows view cannot read itself.
    pub shells: Vec<String>,
}

impl Hello {
    /// This process's own handshake.
    pub fn current() -> Self {
        Self {
            protocol: PROTOCOL_VERSION,
            build: option_env!("CLAUDHUB_BUILD_ID")
                .unwrap_or("dev")
                .to_string(),
            cwd: std::env::current_dir().unwrap_or_default(),
            running_under_wsl: super::watch::running_under_wsl(),
            shells: login_shells(),
        }
    }
}

/// The shells in `/etc/shells` that really exist. Empty outside Unix, which is
/// the right answer: the list is only a suggestion.
fn login_shells() -> Vec<String> {
    let text = std::fs::read_to_string("/etc/shells").unwrap_or_default();
    text.lines()
        .map(str::trim)
        .filter(|line| line.starts_with('/') && Path::new(line).exists())
        .map(str::to_string)
        .collect()
}

/// A guard on a frame's length, applied at **both** ends. An agent-review diff
/// is measured in megabytes, never gigabytes: beyond that, on reading, the
/// stream is desynchronised and those four bytes were not a length.
///
/// The writer honours the same ceiling, and that is the point: it used to send
/// whatever it was given, so a frame the reader refuses was a frame the writer
/// had happily produced — and the wire died on a payload that was merely too
/// big, which the user reads as "server lost" rather than "that diff is
/// enormous". Checking here is also what makes the four-byte length safe to
/// write: past four gigabytes the cast would truncate, and the frame would
/// announce a length that is not its own.
const MAX_FRAME: u32 = 256 * 1024 * 1024;

/// Writes a frame.
///
/// Two payloads never go out: one that does not serialise — a non-UTF-8 path,
/// in practice — and one too big for the other end to read back. Both are
/// logged and dropped rather than returned as an error: losing one event is
/// better than closing the wire for everybody, and the view asks again by
/// itself for everything it is still waiting for.
pub fn write_frame<T: Serialize>(out: &mut impl Write, value: &T) -> std::io::Result<()> {
    write_within(out, value, MAX_FRAME)
}

/// The same, against an arbitrary ceiling. Only the tests pass another one:
/// proving the guard by really building a 256 MB payload would cost more
/// memory than the whole test run.
fn write_within<T: Serialize>(
    out: &mut impl Write,
    value: &T,
    ceiling: u32,
) -> std::io::Result<()> {
    let body = match postcard::to_stdvec(value) {
        Ok(body) => body,
        Err(e) => {
            log::warn!("unserialisable value dropped from the wire: {e}");
            return Ok(());
        }
    };
    write_body(out, &body, ceiling)
}

/// Writes the handshake, and **refuses** what does not serialise.
///
/// The one frame `write_frame`'s rule does not fit: losing an event costs that
/// event, losing a hello costs the wire — each end waits for the other's
/// before anything else, and a hello dropped in silence left both waiting for
/// ever, the window saying neither "connected" nor "lost". The cause in
/// practice is a launch directory whose name is not UTF-8, and the error says
/// so: postcard's own is a bare "serialization error", its `custom` keeping no
/// message.
pub fn write_hello(out: &mut impl Write, hello: &Hello) -> std::io::Result<()> {
    let body = postcard::to_stdvec(hello).map_err(|e| {
        let why = match hello.cwd.to_str() {
            None => format!(
                "the launch directory {} is not UTF-8, which the wire cannot carry",
                hello.cwd.display()
            ),
            Some(_) => e.to_string(),
        };
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("the handshake cannot be sent: {why}"),
        )
    })?;
    write_body(out, &body, MAX_FRAME)
}

/// A serialised body, framed — or dropped when it is over the ceiling.
fn write_body(out: &mut impl Write, body: &[u8], ceiling: u32) -> std::io::Result<()> {
    let Ok(len) = u32::try_from(body.len()) else {
        log::warn!("{}-byte value dropped from the wire", body.len());
        return Ok(());
    };
    if len > ceiling {
        log::warn!("{len}-byte value dropped from the wire: over the {ceiling}-byte ceiling");
        return Ok(());
    }
    out.write_all(&len.to_le_bytes())?;
    out.write_all(body)?;
    // One frame per event, and one flush per frame: the other end sometimes
    // waits for that very answer to draw, and a buffer would hold it back.
    out.flush()
}

/// Reads the next frame. `Ok(None)` is the clean end of the stream — the other
/// end is gone; an absurd length or an unreadable body are errors, the stream
/// is no longer a protocol.
pub fn read_frame<T: DeserializeOwned>(input: &mut impl Read) -> anyhow::Result<Option<T>> {
    let mut len = [0u8; 4];
    match input.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(len);
    if len > MAX_FRAME {
        anyhow::bail!("a {len}-byte frame announced: the stream is desynchronised");
    }
    let mut body = vec![0u8; len as usize];
    input.read_exact(&mut body)?;
    Ok(Some(postcard::from_bytes(&body)?))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use super::*;
    use crate::runtime::protocol::{Cmd, Evt, Order, Secret, Ticket};

    /// A round trip over the wire, frame included.
    fn roundtrip<T: Serialize + DeserializeOwned>(value: &T) -> T {
        let mut buffer = Vec::new();
        write_frame(&mut buffer, value).expect("write");
        read_frame(&mut buffer.as_slice())
            .expect("read")
            .expect("one frame")
    }

    #[test]
    fn a_payload_over_the_ceiling_is_dropped_and_not_written() {
        // The wire's own guard, seen from the writing end. It used to send
        // whatever it was given, and the reader refuses past the ceiling: the
        // pair therefore killed the wire over a payload that was merely too
        // big, which the window reads as "server lost".
        let mut buffer = Vec::new();
        let evt = Evt::Failed {
            ticket: crate::runtime::Ticket(1),
            worktree: None,
            action: crate::runtime::Action::Refresh,
            message: "x".repeat(64),
        };
        write_within(&mut buffer, &evt, 16).expect("write");
        assert!(buffer.is_empty(), "nothing must reach the wire");

        // And what fits still goes through, frame and body.
        write_within(&mut buffer, &evt, MAX_FRAME).expect("write");
        assert!(!buffer.is_empty());
        let back: Evt = read_frame(&mut buffer.as_slice())
            .expect("read")
            .expect("one frame");
        assert!(matches!(back, Evt::Failed { .. }));
    }

    #[test]
    fn a_dropped_frame_does_not_desynchronise_what_follows() {
        // The frame is dropped whole — not half-written — so the next event
        // reads back as itself and not as gibberish.
        let mut buffer = Vec::new();
        let big = Evt::Failed {
            ticket: crate::runtime::Ticket(1),
            worktree: None,
            action: crate::runtime::Action::Refresh,
            message: "x".repeat(64),
        };
        let small = Evt::ServerLost {
            message: String::new(),
        };
        write_within(&mut buffer, &big, 16).expect("write");
        write_within(&mut buffer, &small, 16).expect("write");
        let mut input = buffer.as_slice();
        let back: Evt = read_frame(&mut input).expect("read").expect("one frame");
        assert!(matches!(back, Evt::ServerLost { .. }));
        assert!(read_frame::<Evt>(&mut input).expect("read").is_none());
    }

    /// The cases that decided the format: a map keyed by `PathBuf` (which JSON
    /// refuses), a diff nested three levels deep, a result set, accents in
    /// paths and in text.
    #[test]
    fn the_hard_payloads_cross_the_wire_intact() {
        let mut agents = HashMap::new();
        agents.insert(
            PathBuf::from("/home/zoé/projects/quotes"),
            vec![crate::agent::Process {
                pid: 4242,
                program: "claude".into(),
                cpu: 7,
            }],
        );
        let evt = Evt::Agents {
            agents: agents.clone(),
        };
        match roundtrip(&evt) {
            Evt::Agents { agents: back } => assert_eq!(back, agents),
            other => panic!("wrong variant: {other:?}"),
        }

        let diff = crate::git::FileDiff {
            hunks: vec![crate::git::Hunk {
                header: "@@ -1,2 +1,2 @@".into(),
                old_start: 1,
                new_start: 1,
                lines: vec![crate::git::DiffLine {
                    kind: crate::git::DiffLineKind::Added,
                    text: "élaborate answer".into(),
                    old_no: None,
                    new_no: Some(1),
                    cr: true,
                }],
            }],
            ..Default::default()
        };
        let evt = Evt::FileDiff {
            worktree: PathBuf::from("/tmp/wt"),
            range: crate::git::DiffRange::Branch {
                base: "main".into(),
            },
            path: PathBuf::from("src/élan.rs"),
            diff: diff.clone(),
        };
        match roundtrip(&evt) {
            Evt::FileDiff { diff: back, .. } => assert_eq!(back, diff),
            other => panic!("wrong variant: {other:?}"),
        }

        let rows = crate::db::Rows {
            columns: vec!["id".into(), "name".into()],
            rows: vec![vec![Some("1".into()), None]],
            affected: None,
            offset: 0,
            more: true,
        };
        let evt = Evt::DbRows {
            request: 3,
            rows: Ok(rows.clone()),
            elapsed_ms: 12,
        };
        match roundtrip(&evt) {
            Evt::DbRows { rows: Ok(back), .. } => assert_eq!(back, rows),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    /// The secret crosses the wire but not the traces.
    #[test]
    fn a_secret_crosses_the_wire_but_not_the_logs() {
        let cmd = Cmd::Call {
            caller: crate::runtime::protocol::Caller::Sentry,
            call: 1,
            cap: crate::outside::Cap::Http {
                method: "GET".into(),
                url: "https://sentry.io/api/0/issues/42/".into(),
                headers: vec![("Authorization".into(), "Bearer {secret}".into())],
                body: None,
                secret: Some(Secret("sntrys_hunter2".into())),
            },
        };
        let back = roundtrip(&cmd);
        match back {
            Cmd::Call {
                cap: crate::outside::Cap::Http { secret, .. },
                ..
            } => assert_eq!(secret.expect("the secret travels").0, "sntrys_hunter2"),
            other => panic!("wrong variant: {other:?}"),
        }
        assert!(!format!("{cmd:?}").contains("hunter2"), "{cmd:?}");
    }

    /// Two frames in a row read back in order, and the end of the stream is a
    /// clean `None`, not an error.
    #[test]
    fn frames_follow_each_other_and_eof_is_clean() {
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &Cmd::OpenRepo(PathBuf::from("/a"))).unwrap();
        write_frame(&mut buffer, &Cmd::OpenIfRepo(PathBuf::from("/b"))).unwrap();
        let mut input = buffer.as_slice();
        assert!(matches!(
            read_frame::<Cmd>(&mut input).unwrap(),
            Some(Cmd::OpenRepo(p)) if p == std::path::Path::new("/a")
        ));
        assert!(matches!(
            read_frame::<Cmd>(&mut input).unwrap(),
            Some(Cmd::OpenIfRepo(p)) if p == std::path::Path::new("/b")
        ));
        assert!(read_frame::<Cmd>(&mut input).unwrap().is_none());
    }

    /// A command's ticket crosses with it, and comes back on its answer: it is
    /// all the window has to tell one write from another of the same kind.
    #[test]
    fn a_ticket_crosses_both_ways() {
        let order = Order {
            ticket: Ticket(41),
            cmd: Cmd::Fetch {
                worktree: PathBuf::from("/p"),
            },
        };
        let back = roundtrip(&order);
        assert_eq!(back.ticket, Ticket(41));
        assert!(matches!(back.cmd, Cmd::Fetch { .. }));
        let evt = Evt::Done {
            ticket: Ticket(41),
            worktree: None,
            action: crate::runtime::Action::Fetch,
            output: String::new(),
        };
        assert!(matches!(
            roundtrip(&evt),
            Evt::Done {
                ticket: Ticket(41),
                ..
            }
        ));
    }

    /// A hello that cannot be written is an error and not a dropped frame:
    /// dropped, it left both ends waiting for the other's for ever.
    #[cfg(unix)]
    #[test]
    fn a_hello_that_does_not_serialise_is_an_error() {
        use std::os::unix::ffi::OsStrExt;
        let hello = Hello {
            cwd: PathBuf::from(std::ffi::OsStr::from_bytes(b"/home/z\xffe")),
            ..Hello::current()
        };
        let mut buffer = Vec::new();
        let err = write_hello(&mut buffer, &hello).expect_err("must be refused");
        assert!(err.to_string().contains("not UTF-8"), "{err}");
        assert!(buffer.is_empty(), "nothing half-written");

        // And one that does goes out whole, readable by the other end.
        write_hello(&mut buffer, &Hello::current()).expect("write");
        let back: Hello = read_frame(&mut buffer.as_slice()).unwrap().unwrap();
        assert_eq!(back.protocol, PROTOCOL_VERSION);
    }

    /// An absurd length is an error, not an allocation.
    #[test]
    fn a_desynchronised_stream_is_refused() {
        let mut input: &[u8] = &[0xff, 0xff, 0xff, 0xff];
        assert!(read_frame::<Cmd>(&mut input).is_err());
    }
}
