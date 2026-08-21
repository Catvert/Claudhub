//! `claudhub-server` — les workers de Claudhub, headless.
//!
//! C'est le binaire qui tourne dans la distro WSL2 quand l'interface est un
//! `.exe` Windows : mêmes files, même `runtime::handle`, mais les `Cmd`
//! arrivent par l'entrée standard et les `Evt` repartent par la sortie —
//! **stdout appartient au fil**, les traces vont sur stderr (voir
//! `runtime::wire`). Il se construit sans la feature `ui`, ce qui est le
//! portillon prouvant qu'aucun module du cœur ne tire gpui.
//!
//! Le cycle de vie est celui du parent : la fin de l'entrée standard est
//! l'ordre de s'éteindre, et perdre la sortie standard (le parent est mort
//! sans fermer proprement) l'est aussi.

use claudhub::runtime::{self, wire};

fn main() {
    // Avant tout le reste, et avant qu'un thread existe : c'est ce serveur
    // qui lancera les agents (via wt et commit_msg), et les marqueurs de
    // session de ce qui nous a lancés feraient de chacun une sous-session du
    // sien.
    claudhub::agent::disinherit_session();
    claudhub::logging::init();

    let mut stdout = std::io::stdout().lock();
    // Le verrou de stdin n'est pas `Send` : on garde le poignet et on
    // verrouille dans le thread qui lit.
    let stdin = std::io::stdin();

    // Notre poignée de main part d'abord ; celle du client suit. Les deux
    // bouts écrivent avant de lire : deux petites trames tiennent dans les
    // tampons des tubes, personne ne s'attend mutuellement.
    if let Err(e) = wire::write_frame(&mut stdout, &wire::Hello::current()) {
        eprintln!("claudhub-server : poignée de main impossible : {e}");
        std::process::exit(1);
    }
    let client: wire::Hello = match wire::read_frame(&mut stdin.lock()) {
        Ok(Some(hello)) => hello,
        Ok(None) => std::process::exit(0), // parti avant de se présenter
        Err(e) => {
            eprintln!("claudhub-server : poignée de main illisible : {e}");
            std::process::exit(1);
        }
    };
    // Le client fait la même vérification de son côté ; celle-ci attrape un
    // client trop vieux pour savoir la faire.
    if client.protocol != wire::PROTOCOL_VERSION {
        eprintln!(
            "claudhub-server : versions désaccordées (client {}, serveur {})",
            client.protocol,
            wire::PROTOCOL_VERSION
        );
        std::process::exit(3);
    }

    let (handle, evt_rx) = runtime::spawn();

    // L'entrée : une trame, un `Cmd`, la même remise que celle de la vue —
    // c'est `Handle::send` qui refait le tri entre les files.
    std::thread::Builder::new()
        .name("claudhub-server-in".into())
        .spawn(move || loop {
            match wire::read_frame::<runtime::Cmd>(&mut stdin.lock()) {
                Ok(Some(cmd)) => handle.send(cmd),
                // Fin propre : le parent a fermé le fil, on le suit.
                Ok(None) => std::process::exit(0),
                Err(e) => {
                    eprintln!("claudhub-server : flux d'entrée illisible : {e}");
                    std::process::exit(1);
                }
            }
        })
        .expect("thread d'entrée");

    // La sortie, sur le thread principal : les événements de toutes les
    // files, sérialisés dans l'ordre où ils sortent du canal partagé.
    while let Ok(evt) = evt_rx.recv_blocking() {
        if let Err(e) = wire::write_frame(&mut stdout, &evt) {
            eprintln!("claudhub-server : le parent n'écoute plus : {e}");
            std::process::exit(1);
        }
    }
}
