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

/// La version du protocole. À incrémenter à **chaque** changement de `Cmd`,
/// d'`Evt` ou d'un type qu'ils transportent : les deux bouts sont deux
/// binaires livrés ensemble mais installés séparément, et un désaccord doit
/// se dire à la poignée de main plutôt qu'en trame illisible au premier diff.
pub const PROTOCOL_VERSION: u32 = 1;

/// La première trame de chaque bout, avant tout `Cmd` ou `Evt`.
///
/// Elle est **hors** des deux grandes énumérations, et ses champs ne se
/// réordonnent jamais : c'est elle qui détecte un désaccord de version, elle
/// doit donc se relire depuis n'importe quelle version — postcard est
/// positionnel, et un enum qui a bougé décode du charabia sans le dire.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Hello {
    pub protocol: u32,
    /// L'identifiant de build (`CLAUDHUB_BUILD_ID` en CI, `dev` ailleurs) :
    /// pour les traces, la version qui fait foi est `protocol`.
    pub build: String,
    /// Le répertoire de lancement du serveur : c'est lui qui remplace le
    /// `current_dir` de la vue quand les workers tournent ailleurs.
    pub cwd: PathBuf,
    /// Vrai si le serveur tourne sous WSL — c'est là que « ce chemin est un
    /// montage Windows » a un sens, et la vue ne peut pas le deviner.
    pub running_under_wsl: bool,
    /// Les shells de connexion du serveur (`/etc/shells`) : la liste que le
    /// formulaire des réglages propose, et qu'une vue Windows ne peut pas
    /// lire elle-même.
    pub shells: Vec<String>,
}

impl Hello {
    /// La poignée de main de ce processus-ci.
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

/// Garde-fou sur la longueur annoncée d'une trame. Un diff de relecture
/// d'agent se compte en méga-octets, jamais en giga-octets : au-delà, c'est
/// que le flux est désynchronisé et que ces quatre octets n'étaient pas une
/// longueur.
const MAX_FRAME: u32 = 256 * 1024 * 1024;

/// Écrit une trame. Une valeur qui ne se sérialise pas — un chemin non-UTF-8,
/// en pratique — est journalisée et écartée : perdre un événement vaut mieux
/// que fermer le fil pour tout le monde.
pub fn write_frame<T: Serialize>(out: &mut impl Write, value: &T) -> std::io::Result<()> {
    let body = match postcard::to_stdvec(value) {
        Ok(body) => body,
        Err(e) => {
            log::warn!("valeur insérialisable écartée du fil : {e}");
            return Ok(());
        }
    };
    out.write_all(&(body.len() as u32).to_le_bytes())?;
    out.write_all(&body)?;
    // Une trame par événement, et un flush par trame : l'autre bout attend
    // parfois cette réponse-là pour dessiner, et un tampon la retiendrait.
    out.flush()
}

/// Lit la trame suivante. `Ok(None)` est la fin propre du flux — l'autre bout
/// est parti ; une longueur aberrante ou un corps illisible sont des erreurs,
/// le flux n'est plus un protocole.
pub fn read_frame<T: DeserializeOwned>(input: &mut impl Read) -> anyhow::Result<Option<T>> {
    let mut len = [0u8; 4];
    match input.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(len);
    if len > MAX_FRAME {
        anyhow::bail!("trame de {len} octets annoncée : le flux est désynchronisé");
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

    /// Aller-retour par le fil, trame comprise.
    fn roundtrip<T: Serialize + DeserializeOwned>(value: &T) -> T {
        let mut buffer = Vec::new();
        write_frame(&mut buffer, value).expect("écriture");
        read_frame(&mut buffer.as_slice())
            .expect("lecture")
            .expect("une trame")
    }

    /// Les cas qui ont décidé du format : une table à clés `PathBuf` (que
    /// JSON refuse), un diff imbriqué sur trois niveaux, un résultat, des
    /// accents dans les chemins et les textes.
    #[test]
    fn the_hard_payloads_cross_the_wire_intact() {
        let mut agents = HashMap::new();
        agents.insert(
            PathBuf::from("/home/aurélie/projets/devis"),
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
            other => panic!("mauvaise variante : {other:?}"),
        }

        let diff = crate::git::FileDiff {
            hunks: vec![crate::git::Hunk {
                header: "@@ -1,2 +1,2 @@".into(),
                old_start: 1,
                new_start: 1,
                lines: vec![crate::git::DiffLine {
                    kind: crate::git::DiffLineKind::Added,
                    text: "réponse élaborée".into(),
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
            other => panic!("mauvaise variante : {other:?}"),
        }

        let rows = crate::db::Rows {
            columns: vec!["id".into(), "nom".into()],
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
            other => panic!("mauvaise variante : {other:?}"),
        }
    }

    /// Le secret traverse le fil mais pas les traces.
    #[test]
    fn a_secret_crosses_the_wire_but_not_the_logs() {
        let cmd = Cmd::LoadIssueEvent {
            issue: "42".into(),
            token: Secret("sntrys_tout_un_jeton".into()),
        };
        let back = roundtrip(&cmd);
        match back {
            Cmd::LoadIssueEvent { token, .. } => assert_eq!(token.0, "sntrys_tout_un_jeton"),
            other => panic!("mauvaise variante : {other:?}"),
        }
        assert!(!format!("{cmd:?}").contains("jeton"), "{cmd:?}");
    }

    /// Deux trames à la suite se relisent dans l'ordre, et la fin du flux est
    /// un `None` propre, pas une erreur.
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
