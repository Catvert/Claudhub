//! Embarque le serveur headless dans l'exécutable de l'interface.
//!
//! Sous Windows, les workers tournent dans une distribution WSL2 et il faut y
//! poser un binaire. Le livrer *à côté* de l'exécutable marchait, mais laissait
//! deux fichiers dans l'archive dont un que personne ne sait quoi faire — et
//! rien n'empêchait de garder un vieux serveur à côté d'une interface neuve.
//!
//! `CLAUDHUB_EMBED_SERVER` désigne le binaire à embarquer ; la CI le pose après
//! avoir construit la cible musl. Sans lui — c'est le cas de tout build de
//! développement, qui n'a pas croisé-compilé quoi que ce soit — la constante
//! vaut `None` et `wsl::ensure_installed` retombe sur le fichier voisin.
//!
//! Le chemin est écrit par `{:?}`, qui rend un littéral Rust échappé : sous
//! Windows il contient des antislashs, et les recopier tels quels donnerait des
//! séquences d'échappement au milieu du chemin.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=CLAUDHUB_EMBED_SERVER");

    let generated = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("server.rs");
    let body = match std::env::var_os("CLAUDHUB_EMBED_SERVER") {
        Some(path) => {
            let path = PathBuf::from(path);
            // A hard error rather than a silent fallback: the variable was set
            // on purpose, and an executable shipped without its server is
            // exactly what this is meant to make impossible.
            assert!(
                path.is_file(),
                "CLAUDHUB_EMBED_SERVER names {}, which is not a file",
                path.display()
            );
            println!("cargo:rerun-if-changed={}", path.display());
            format!("pub const EMBEDDED: Option<&[u8]> = Some(include_bytes!({path:?}));\n")
        }
        None => "pub const EMBEDDED: Option<&[u8]> = None;\n".to_string(),
    };
    std::fs::write(&generated, body).expect("writing the embedded server");
}
