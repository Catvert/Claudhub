//! La chaîne complète du transport distant, sur cette machine.
//!
//! C'est le portillon du mode serveur : le vrai binaire `claudhub-server`
//! (cargo le construit pour ce test), la vraie poignée de main, un `Cmd` qui
//! descend et l'`Evt` qui remonte. La fenêtre n'est pas là, mais tout ce qui
//! la sépare du serveur l'est — `CLAUDHUB_SERVER_CMD` branche exactement ce
//! chemin dans l'application.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use claudhub::runtime::remote::connect;
use claudhub::runtime::{Cmd, Evt};

/// Le prochain événement, avec l'impatience d'un test : le canal est drainé
/// par une pompe gpui dans l'application, ici par une boucle courte.
fn next(events: &async_channel::Receiver<Evt>) -> Evt {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        match events.try_recv() {
            Ok(evt) => return evt,
            Err(async_channel::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(async_channel::TryRecvError::Closed) => panic!("le fil s'est fermé"),
        }
    }
    panic!("aucun événement du serveur en trente secondes");
}

#[test]
fn the_server_answers_over_the_wire() {
    let argv = vec![env!("CARGO_BIN_EXE_claudhub-server").to_string()];
    let (handle, events) = connect(&argv).expect("lancement du serveur");

    // La poignée de main d'abord, toujours : versions accordées, et ce que
    // le serveur sait de sa machine.
    match next(&events) {
        Evt::ServerHello { cwd, .. } => assert!(cwd.is_absolute(), "{cwd:?}"),
        other => panic!("attendu ServerHello, reçu {other:?}"),
    }

    // Un ordre qui traverse tout : la trame, les files du serveur, le worker
    // git, et la réponse — un dépôt qui n'existe pas répond proprement.
    let nowhere = PathBuf::from("/claudhub-nulle-part");
    handle.send(Cmd::OpenRepo(nowhere.clone()));
    loop {
        match next(&events) {
            Evt::RepoUnavailable { path, message } => {
                assert_eq!(path, nowhere);
                assert!(!message.is_empty());
                break;
            }
            // Le surveillant du serveur peut parler entre-temps ; il ne
            // s'agit pas de lui.
            _ => continue,
        }
    }
    // Lâcher le manche ferme l'entrée du serveur, qui s'éteint : c'est le
    // cycle de vie nominal, et un serveur qui survivrait deviendrait un
    // processus orphelin par test.
    drop(handle);
}

#[test]
fn a_dead_server_is_reported_not_swallowed() {
    // Un « serveur » qui meurt avant de se présenter : c'est aussi ce que
    // voit la vue quand on tue le vrai en plein vol, et la réponse doit être
    // un événement qu'elle peut afficher, jamais un silence.
    let (_handle, events) = connect(&["true".to_string()]).expect("lancement");
    match next(&events) {
        Evt::ServerLost { message } => assert!(!message.is_empty()),
        other => panic!("attendu ServerLost, reçu {other:?}"),
    }
}
