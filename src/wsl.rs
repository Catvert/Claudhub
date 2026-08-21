//! Le serveur dans la distro : l'y trouver, l'y installer, l'y lancer.
//!
//! C'est la moitié Windows de la découpe. L'interface est un `.exe` natif, les
//! workers tournent dans WSL2, et entre les deux il faut que le binaire du
//! serveur *soit* dans la distro — l'utilisateur n'ayant aucune raison de l'y
//! mettre à la main. Il est donc livré à côté de l'exécutable et copié à la
//! première ouverture, comme le fait VS Code.
//!
//! **L'installation est adressée par le contenu**, jamais par un numéro de
//! version : l'empreinte du binaire livré nomme son dossier. Deux `.exe`
//! différents installent donc deux serveurs différents sans se marcher dessus,
//! une mise à jour s'installe d'elle-même, et une version de développement —
//! qui n'a pas de numéro — se comporte comme les autres. C'est le motif de
//! `tools/make_appimage.sh`, pour la même raison.
//!
//! **Rien ici ne passe par un shell de connexion.** `wsl.exe --exec` lance le
//! programme directement : ce qui s'y écrit doit donc être un chemin absolu,
//! et non un `~` que personne ne développerait. D'où [`probe`], qui demande
//! une fois pour toutes à la distro où est le foyer de l'utilisateur et quel
//! shell lui appartient.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

/// Le nom du binaire, le même des deux côtés du fil.
pub const SERVER_BIN: &str = "claudhub-server";

// `EMBEDDED` : le serveur embarqué dans l'exécutable, quand il y en a un.
// Posé par `build.rs` d'après `CLAUDHUB_EMBED_SERVER` — présent dans ce que la
// CI livre, absent d'un build de développement, qui n'a pas de binaire musl
// sous la main. Voir `bundled_server` pour le repli.
include!(concat!(env!("OUT_DIR"), "/server.rs"));

/// Ce que la distro nous apprend d'elle-même, en un seul aller-retour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// Le foyer de l'utilisateur, en absolu : `--exec` ne développe pas `~`.
    pub home: String,
    /// Son shell de connexion, tel que `/etc/passwd` le déclare.
    ///
    /// Il est demandé ici parce que c'est le seul endroit où on peut : un
    /// terminal lancé par `--exec` n'a pas de shell pour interroger `$SHELL`,
    /// et c'est précisément un shell qu'on veut lancer.
    pub shell: String,
}

/// Les distributions installées, dans l'ordre où `wsl.exe` les donne.
///
/// La première est celle par défaut, ce qui en fait une proposition
/// raisonnable quand il faut en choisir une.
pub fn distributions() -> Result<Vec<String>> {
    let out = wsl()
        .args(["--list", "--quiet"])
        .output()
        .context("wsl.exe est introuvable : WSL est-il installé ?")?;
    let text = decode(&out.stdout);
    let list: Vec<String> = text
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();
    if list.is_empty() {
        bail!("aucune distribution WSL installée");
    }
    Ok(list)
}

/// Demande à la distro où vit l'utilisateur et quel shell lui appartient.
pub fn probe(distro: &str) -> Result<Probe> {
    // `getent` plutôt que `$SHELL` : la variable n'est posée que par un shell
    // de connexion, et il n'y en a pas ici. Le champ 7 de `passwd` fait foi.
    let out = run(
        distro,
        "printf %s\\\\n $HOME; getent passwd $(id -u) | cut -d: -f7",
    )?;
    let mut lines = out.lines();
    let home = lines.next().unwrap_or_default().trim().to_string();
    let shell = lines.next().unwrap_or_default().trim().to_string();
    if home.is_empty() {
        bail!("la distribution « {distro} » n'a pas dit où est le foyer de l'utilisateur");
    }
    Ok(Probe {
        home,
        // Un foyer sans shell déclaré est improbable, mais `sh` existe partout.
        shell: if shell.is_empty() {
            "/bin/sh".into()
        } else {
            shell
        },
    })
}

/// Le binaire serveur livré à côté de l'exécutable.
///
/// Le repli du build de développement : ce qui est livré porte son serveur
/// dedans (voir `EMBEDDED`), mais une compilation locale n'a pas de binaire
/// musl à embarquer, et poser le fichier à côté reste la façon de s'en donner
/// un.
pub fn bundled_server() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("chemin de l'exécutable introuvable")?;
    let path = exe
        .parent()
        .map(|dir| dir.join(SERVER_BIN))
        .unwrap_or_default();
    if !path.is_file() {
        bail!(
            "cet exécutable ne porte pas de serveur embarqué et {SERVER_BIN} \
             est absent d'à côté de lui ({}) : construire la cible musl et \
             poser le binaire là, ou passer par CLAUDHUB_SERVER_CMD",
            path.display()
        );
    }
    Ok(path)
}

/// Les octets du serveur à installer : ceux de l'exécutable s'il en porte,
/// ceux du fichier voisin sinon.
fn server_bytes() -> Result<Vec<u8>> {
    if let Some(bytes) = EMBEDDED {
        return Ok(bytes.to_vec());
    }
    let source = bundled_server()?;
    std::fs::read(&source).with_context(|| format!("lecture de {} impossible", source.display()))
}

/// Installe le serveur dans la distro s'il n'y est pas déjà, et rend son
/// chemin absolu.
///
/// L'octroi du bit d'exécution n'est pas une précaution : une archive zip ne
/// le transporte pas, et c'est la panne que tout le monde rencontre en
/// copiant le binaire à la main.
pub fn ensure_installed(distro: &str, probe: &Probe) -> Result<String> {
    let bytes = server_bytes()?;
    let id = content_id(&bytes);
    let dir = format!("{}/.claudhub/bin/{id}", probe.home);
    let target = format!("{dir}/{SERVER_BIN}");

    if run(distro, &format!("test -x {target} && echo ok")).is_ok_and(|out| out.trim() == "ok") {
        return Ok(target);
    }

    // Écrit à côté puis renommé : un `mv` est atomique, si bien qu'une
    // installation interrompue ne laisse pas un binaire tronqué que le
    // lancement suivant prendrait pour bon.
    let script = install_script(&dir, &target, &probe.home, &id);
    let mut child = wsl()
        .args(["-d", distro, "--exec", "/bin/sh", "-c", &script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("wsl.exe n'a pas pu lancer l'installation")?;
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().expect("stdin demandé");
        stdin
            .write_all(&bytes)
            .context("envoi du serveur dans la distribution")?;
    }
    let out = child
        .wait_with_output()
        .context("installation du serveur")?;
    if !out.status.success() {
        bail!(
            "installation du serveur dans « {distro} » : {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(target)
}

/// Le script d'installation, écrit sans le moindre guillemet.
///
/// C'est délibéré : la ligne traverse `CreateProcess`, puis `wsl.exe`, qui
/// reconstruit l'`argv` à sa façon — chaque guillemet y est une occasion de
/// se faire manger. Le prix est qu'un foyer contenant une espace ne marcherait
/// pas ; on n'en a jamais vu sous Linux.
///
/// La purge garde le dossier du build courant et jette les autres : sans elle,
/// chaque mise à jour laisserait douze mégaoctets derrière elle.
fn install_script(dir: &str, target: &str, home: &str, id: &str) -> String {
    format!(
        "set -e; mkdir -p {dir}; cat > {target}.part; chmod +x {target}.part; \
         mv {target}.part {target}; \
         find {home}/.claudhub/bin -mindepth 1 -maxdepth 1 ! -name {id} -exec rm -rf {{}} +"
    )
}

/// La ligne de commande qui lance le serveur dans la distro.
///
/// `--cd` n'est pas un confort : le serveur annonce son répertoire de
/// démarrage dans sa poignée de main, et c'est ce qui ouvre le dépôt d'où
/// l'on vient — le « lancé depuis son projet » du mode local.
pub fn launch_argv(distro: &str, server: &str, cwd: Option<&str>) -> Vec<String> {
    let mut argv = vec!["wsl.exe".into(), "-d".into(), distro.to_string()];
    if let Some(cwd) = cwd.filter(|c| !c.is_empty()) {
        argv.push("--cd".into());
        argv.push(cwd.to_string());
    }
    argv.push("--exec".into());
    argv.push(server.to_string());
    argv
}

/// La ligne de commande d'un onglet de terminal, à travers `wsl.exe`.
///
/// Sous Windows, les dépôts vivent dans la distribution : un terminal qui
/// s'ouvrirait localement regarderait un chemin qui n'existe pas, et l'agent
/// qu'on y lance ne verrait pas le code. Le pty, lui, reste local — c'est
/// ConPTY qui le porte, et l'émulation ne change pas d'un octet.
///
/// L'environnement passe par `/usr/bin/env` et non par des variables du
/// processus Windows : ce qui compte est ce que voit le processus **Linux**,
/// et `wsl.exe` ne transmet pas l'environnement de son appelant.
/// `--exec` évite qu'un shell intermédiaire re-découpe les arguments.
pub fn terminal_argv(
    distro: &str,
    cwd: &str,
    login_shell: &str,
    command: Option<(String, Vec<String>)>,
    env: &[(String, String)],
) -> (String, Vec<String>) {
    let mut args = vec!["-d".to_string(), distro.to_string()];
    if !cwd.is_empty() {
        args.push("--cd".into());
        args.push(cwd.to_string());
    }
    args.push("--exec".into());
    args.push("/usr/bin/env".into());
    for (key, value) in env {
        args.push(format!("{key}={value}"));
    }
    match command {
        Some((program, rest)) => {
            args.push(program);
            args.extend(rest);
        }
        // Le shell de connexion, et **en connexion** (`-l`) : c'est ce que
        // fait tout terminal, et c'est ce qui lit `.profile` — sans quoi le
        // `PATH` de l'utilisateur manquerait la moitié de ses outils, dont
        // souvent l'agent qu'il veut lancer.
        None => {
            args.push(login_shell.to_string());
            args.push("-l".into());
        }
    }
    ("wsl.exe".to_string(), args)
}

/// L'empreinte d'un contenu, en hexadécimal — FNV-1a 64 bits, comme celle des
/// fichiers ouverts, et pour la même raison : elle doit être la même d'un
/// binaire à l'autre.
pub fn content_id(bytes: &[u8]) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

/// Lance une commande shell dans la distro et rend sa sortie standard.
fn run(distro: &str, script: &str) -> Result<String> {
    let out = wsl()
        .args(["-d", distro, "--exec", "/bin/sh", "-c", script])
        .output()
        .context("wsl.exe est introuvable : WSL est-il installé ?")?;
    if !out.status.success() {
        bail!(
            "« {distro} » a refusé : {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(decode(&out.stdout))
}

fn wsl() -> Command {
    let mut cmd = Command::new("wsl.exe");
    // Depuis WSL 0.64, cette variable rend les sorties de `wsl.exe` en UTF-8 ;
    // sans elle, `--list` répond en UTF-16, d'où le repli de `decode`.
    cmd.env("WSL_UTF8", "1");
    cmd
}

/// Décode ce que `wsl.exe` écrit, en UTF-8 comme en UTF-16.
///
/// Les versions d'avant `WSL_UTF8` répondent en UTF-16 petit-boutiste, ce qui
/// donne, lu comme de l'UTF-8, un nom sur deux caractères et un octet nul
/// entre chaque lettre — une liste de distributions parfaitement illisible.
fn decode(bytes: &[u8]) -> String {
    let body = bytes.strip_prefix(&[0xFF, 0xFE]).unwrap_or(bytes);
    let nuls = body.iter().filter(|b| **b == 0).count();
    if body.len() >= 2 && nuls * 4 >= body.len() {
        let units: Vec<u16> = body
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(body).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ce qu'on embarque doit être le binaire Linux, pas l'exécutable Windows
    /// ni un fichier de trace ramassé au passage : une erreur de chemin dans
    /// la CI ne se verrait qu'au premier démarrage sur la machine d'un
    /// utilisateur, et se lirait comme une distribution cassée.
    #[test]
    fn an_embedded_server_is_a_linux_binary() {
        let Some(bytes) = EMBEDDED else {
            return; // build de développement : rien n'est embarqué
        };
        assert_eq!(
            &bytes[..4],
            b"\x7fELF",
            "le serveur embarqué n'est pas un ELF"
        );
    }

    #[test]
    fn utf16_output_is_read_like_utf8_output() {
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain(
                "Ubuntu\nDebian\n"
                    .encode_utf16()
                    .flat_map(|u| u.to_le_bytes()),
            )
            .collect();
        assert_eq!(decode(&utf16), "Ubuntu\nDebian\n");
        assert_eq!(decode(b"Ubuntu\nDebian\n"), "Ubuntu\nDebian\n");
        // Un nom accentué ne doit pas se perdre dans la conversion.
        let accented: Vec<u8> = "Ubuntu-Préféré\n"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        assert_eq!(decode(&accented), "Ubuntu-Préféré\n");
    }

    /// Un contenu identique donne la même empreinte, un contenu différent une
    /// autre : c'est tout ce que l'adressage par contenu demande.
    #[test]
    fn the_content_names_the_install() {
        assert_eq!(content_id(b"un serveur"), content_id(b"un serveur"));
        assert_ne!(content_id(b"un serveur"), content_id(b"un autre"));
        assert_eq!(content_id(b"").len(), 16);
    }

    /// Le script ne doit pas contenir de guillemets : ils ne survivent pas au
    /// passage par `wsl.exe`, et c'est la panne qu'on ne comprend qu'après
    /// l'avoir vécue.
    #[test]
    fn the_install_script_carries_no_quotes() {
        let script = install_script(
            "/home/a/.claudhub/bin/ff",
            "/home/a/.claudhub/bin/ff/claudhub-server",
            "/home/a",
            "ff",
        );
        assert!(!script.contains('"'), "{script}");
        assert!(!script.contains('\''), "{script}");
        // Le renommage atomique, et la purge de ce qui n'est plus le build
        // courant.
        assert!(script.contains(".part"), "{script}");
        assert!(script.contains("! -name ff"), "{script}");
    }

    /// Un onglet ordinaire ouvre le shell de connexion dans le worktree, avec
    /// ce que l'agent a besoin de savoir dans son environnement.
    #[test]
    fn a_terminal_opens_the_login_shell_where_the_work_is() {
        let env = [("CLAUDHUB_WORKTREE".to_string(), "/home/a/p".to_string())];
        let (program, args) = terminal_argv("Ubuntu", "/home/a/p", "/bin/zsh", None, &env);
        assert_eq!(program, "wsl.exe");
        assert_eq!(
            args,
            vec![
                "-d",
                "Ubuntu",
                "--cd",
                "/home/a/p",
                "--exec",
                "/usr/bin/env",
                "CLAUDHUB_WORKTREE=/home/a/p",
                "/bin/zsh",
                "-l"
            ]
        );
    }

    /// Une commande explicite — un agent, une tâche `wt` — passe entière, ses
    /// arguments compris : c'est elle qu'on a demandé à lancer.
    #[test]
    fn an_explicit_command_keeps_its_arguments() {
        let command = Some((
            "sh".to_string(),
            vec!["-lc".to_string(), "composer install && exit".to_string()],
        ));
        let (_, args) = terminal_argv("Ubuntu", "/home/a/p", "/bin/sh", command, &[]);
        // L'argument composé reste **un seul** élément : le re-découper le
        // ferait exécuter de travers.
        assert_eq!(args.last().unwrap(), "composer install && exit");
        assert_eq!(args[args.len() - 3], "sh");
        assert!(!args.contains(&"-l".to_string()));
    }

    #[test]
    fn the_launch_line_carries_the_working_directory_when_there_is_one() {
        assert_eq!(
            launch_argv("Ubuntu", "/home/a/s", Some("/home/a/projet")),
            vec![
                "wsl.exe",
                "-d",
                "Ubuntu",
                "--cd",
                "/home/a/projet",
                "--exec",
                "/home/a/s"
            ]
        );
        assert_eq!(
            launch_argv("Ubuntu", "/home/a/s", None),
            vec!["wsl.exe", "-d", "Ubuntu", "--exec", "/home/a/s"]
        );
        // Une chaîne vide n'est pas un répertoire : elle ferait un `--cd`
        // sans argument, et `wsl.exe` avalerait `--exec` à sa place.
        assert_eq!(
            launch_argv("Ubuntu", "/home/a/s", Some("")),
            vec!["wsl.exe", "-d", "Ubuntu", "--exec", "/home/a/s"]
        );
    }
}
