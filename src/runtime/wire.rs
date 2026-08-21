//! Le fil entre l'interface et `claudhub-server` : trames et format.
//!
//! Une trame est une longueur sur quatre octets petit-boutistes puis un corps
//! postcard. Les `Cmd` descendent par l'entrée standard du serveur, les `Evt`
//! remontent par sa sortie standard — **stdout appartient au fil** : un
//! `println!` dans du code worker le corromprait, les traces vont sur stderr.
//!
//! Les secrets voyagent en clair : le fil est un tube privé vers un processus
//! enfant de la même session, pas un réseau. C'est le même niveau de
//! confiance qu'une variable d'environnement.
//!
//! Un chemin non-UTF-8 ne se sérialise pas (c'est `PathBuf` qui refuse, quel
//! que soit le format) : l'envoi le journalise et l'écarte plutôt que de
//! fermer le fil — voir [`write_frame`].

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;

/// The protocol version. To be incremented on **every** change to `Cmd`, to
/// `Evt` or to a type they carry: the two ends are two binaries shipped
/// together but installed separately, and a disagreement should be told at the
/// handshake rather than as an unreadable frame on the first diff.
pub const PROTOCOL_VERSION: u32 = 2;

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

/// Les shells de `/etc/shells` qui existent vraiment. Vide ailleurs que sous
/// Unix, ce qui est la bonne réponse : la liste n'est qu'une suggestion.
fn login_shells() -> Vec<String> {
    let text = std::fs::read_to_string("/etc/shells").unwrap_or_default();
    text.lines()
        .map(str::trim)
        .filter(|line| line.starts_with('/') && Path::new(line).exists())
        .map(str::to_string)
        .collect()
}

/// A guard on a frame's announced length. An agent-review diff is measured in
/// megabytes, never gigabytes: beyond that, the stream is desynchronised and
/// those four bytes were not a length.
const MAX_FRAME: u32 = 256 * 1024 * 1024;

/// Writes a frame. A value that does not serialise — a non-UTF-8 path, in
/// practice — is logged and dropped: losing one event is better than closing
/// the wire for everybody.
pub fn write_frame<T: Serialize>(out: &mut impl Write, value: &T) -> std::io::Result<()> {
    let body = match postcard::to_stdvec(value) {
        Ok(body) => body,
        Err(e) => {
            log::warn!("unserialisable value dropped from the wire: {e}");
            return Ok(());
        }
    };
    out.write_all(&(body.len() as u32).to_le_bytes())?;
    out.write_all(&body)?;
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
    use crate::runtime::protocol::{Cmd, Evt, Secret};

    /// A round trip over the wire, frame included.
    fn roundtrip<T: Serialize + DeserializeOwned>(value: &T) -> T {
        let mut buffer = Vec::new();
        write_frame(&mut buffer, value).expect("write");
        read_frame(&mut buffer.as_slice())
            .expect("read")
            .expect("one frame")
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
                }],
            }],
            ..Default::default()
        };
        let evt = Evt::FileDiff {
            worktree: PathBuf::from("/tmp/wt"),
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
        let cmd = Cmd::LoadIssueEvent {
            issue: "42".into(),
            token: Secret("sntrys_hunter2".into()),
        };
        let back = roundtrip(&cmd);
        match back {
            Cmd::LoadIssueEvent { token, .. } => assert_eq!(token.0, "sntrys_hunter2"),
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

    /// Une longueur aberrante est une erreur, pas une allocation.
    #[test]
    fn a_desynchronised_stream_is_refused() {
        let mut input: &[u8] = &[0xff, 0xff, 0xff, 0xff];
        assert!(read_frame::<Cmd>(&mut input).is_err());
    }
}
