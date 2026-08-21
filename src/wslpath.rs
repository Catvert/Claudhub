//! Traduction des chemins entre Windows et la distro WSL.
//!
//! Le fil ne transporte que des chemins Linux : le serveur vit dans la
//! distro, et c'est son disque qui fait foi. La traduction n'existe donc
//! qu'aux rares endroits où un chemin **entre** côté Windows — le sélecteur
//! de dossier, la cible d'un export CSV — et à celui où un chemin Linux doit
//! **sortir** vers le bureau Windows (ouvrir le coffre dans l'explorateur).
//!
//! Tout est textuel et pur : sous Linux un `\\wsl.localhost\…` n'est qu'un
//! composant unique, et c'est précisément pourquoi ces fonctions travaillent
//! sur la chaîne — elles se testent ainsi sur n'importe quelle machine.

use std::path::{Path, PathBuf};

/// Un chemin Windows ramené au monde Linux.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Translated {
    /// La distro que le chemin nomme, quand il en nomme une
    /// (`\\wsl.localhost\<distro>\…`). C'est à l'appelant de vérifier que
    /// c'est bien celle où le serveur tourne — ouvrir le dépôt d'une autre
    /// distro dans ce serveur-ci trouverait un dossier vide.
    pub distro: Option<String>,
    pub path: PathBuf,
}

/// Traduit un chemin Windows en chemin de la distro. `None` pour ce qui n'a
/// pas d'équivalent — un partage réseau, un chemin déjà Linux.
///
/// Deux formes se traduisent : `\\wsl.localhost\<d>\…` (et son ancêtre
/// `\\wsl$\<d>\…`) vers `/…`, et `C:\…` vers `/mnt/c/…` — les montages drvfs
/// que WSL pose par défaut, lents mais réels, ce qui suffit pour la cible
/// d'un export.
pub fn to_linux(path: &Path) -> Option<Translated> {
    let text = path.to_str()?;
    if let Some(rest) = strip_wsl_prefix(text) {
        let mut parts = rest.split(['\\', '/']).filter(|p| !p.is_empty());
        let distro = parts.next()?.to_string();
        let mut linux = String::new();
        for part in parts {
            linux.push('/');
            linux.push_str(part);
        }
        if linux.is_empty() {
            linux.push('/');
        }
        return Some(Translated {
            distro: Some(distro),
            path: PathBuf::from(linux),
        });
    }
    let mut chars = text.chars();
    let drive = chars.next()?;
    if drive.is_ascii_alphabetic() && chars.next() == Some(':') {
        let rest = chars.as_str();
        if !rest.is_empty() && !rest.starts_with(['\\', '/']) {
            return None; // `C:relatif` : relatif au dossier courant du lecteur
        }
        let mut linux = format!("/mnt/{}", drive.to_ascii_lowercase());
        for part in rest.split(['\\', '/']).filter(|p| !p.is_empty()) {
            linux.push('/');
            linux.push_str(part);
        }
        return Some(Translated {
            distro: None,
            path: PathBuf::from(linux),
        });
    }
    None
}

/// Le chemin tel que le serveur le comprendra.
///
/// Un chemin déjà écrit à la façon de Linux passe tel quel — c'est le cas du
/// coffre qu'on pointe soi-même sur `/home/…` —, un chemin Windows est
/// traduit, et ce qui n'a pas d'équivalent rend `None` : un partage réseau
/// n'est atteignable d'aucun des deux côtés, et le taire donnerait un dossier
/// vide sans dire pourquoi.
pub fn for_server(path: &Path) -> Option<PathBuf> {
    if path.to_string_lossy().starts_with('/') {
        return Some(path.to_path_buf());
    }
    to_linux(path).map(|translated| translated.path)
}

/// Traduit un chemin de la distro en chemin Windows : `/mnt/c/…` redevient
/// `C:\…`, tout le reste passe par le partage `\\wsl.localhost\<distro>\…`.
pub fn to_windows(path: &Path, distro: &str) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix("/mnt/") {
        let mut parts = rest.split('/').filter(|p| !p.is_empty());
        if let Some(drive) = parts.next() {
            let mut chars = drive.chars();
            if let (Some(letter), None) = (chars.next(), chars.next()) {
                if letter.is_ascii_alphabetic() {
                    let mut out = format!("{}:", letter.to_ascii_uppercase());
                    for part in parts {
                        out.push('\\');
                        out.push_str(part);
                    }
                    if out.len() == 2 {
                        out.push('\\');
                    }
                    return PathBuf::from(out);
                }
            }
        }
    }
    let mut out = format!("\\\\wsl.localhost\\{distro}");
    for part in text.split('/').filter(|p| !p.is_empty()) {
        out.push('\\');
        out.push_str(part);
    }
    PathBuf::from(out)
}

/// La part après `\\wsl.localhost\` ou `\\wsl$\`, quelle que soit la casse —
/// les chemins Windows n'en ont pas.
fn strip_wsl_prefix(text: &str) -> Option<&str> {
    for prefix in [
        "\\\\wsl.localhost\\",
        "\\\\wsl$\\",
        "//wsl.localhost/",
        "//wsl$/",
    ] {
        if text.len() >= prefix.len() && text[..prefix.len()].eq_ignore_ascii_case(prefix) {
            return Some(&text[prefix.len()..]);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wsl_share_becomes_a_linux_path() {
        let t = to_linux(Path::new(r"\\wsl.localhost\Ubuntu\home\aurélie\projets")).unwrap();
        assert_eq!(t.distro.as_deref(), Some("Ubuntu"));
        assert_eq!(t.path, PathBuf::from("/home/aurélie/projets"));

        // L'ancienne forme, et la casse de l'explorateur.
        let t = to_linux(Path::new(r"\\WSL$\Debian\srv")).unwrap();
        assert_eq!(t.distro.as_deref(), Some("Debian"));
        assert_eq!(t.path, PathBuf::from("/srv"));
    }

    #[test]
    fn a_drive_becomes_a_drvfs_mount() {
        let t = to_linux(Path::new(r"C:\Users\Arno\export.csv")).unwrap();
        assert_eq!(t.distro, None);
        assert_eq!(t.path, PathBuf::from("/mnt/c/Users/Arno/export.csv"));
    }

    #[test]
    fn what_has_no_linux_side_stays_none() {
        assert_eq!(to_linux(Path::new(r"\\serveur\partage\doc")), None);
        assert_eq!(to_linux(Path::new("/home/deja/linux")), None);
        assert_eq!(to_linux(Path::new("C:relatif")), None);
    }

    #[test]
    fn the_way_back_mirrors_the_way_in() {
        assert_eq!(
            to_windows(Path::new("/home/aurélie/coffre"), "Ubuntu"),
            PathBuf::from(r"\\wsl.localhost\Ubuntu\home\aurélie\coffre")
        );
        assert_eq!(
            to_windows(Path::new("/mnt/c/Users/Arno"), "Ubuntu"),
            PathBuf::from(r"C:\Users\Arno")
        );
        // L'aller-retour d'un chemin de coffre : ce que le sélecteur rend
        // doit redonner ce que le fil transporte.
        let round = to_linux(&to_windows(Path::new("/var/www"), "Ubuntu")).unwrap();
        assert_eq!(round.path, PathBuf::from("/var/www"));
        assert_eq!(round.distro.as_deref(), Some("Ubuntu"));
    }
}
