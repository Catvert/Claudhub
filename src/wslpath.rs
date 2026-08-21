//! Path translation between Windows and the WSL distribution.
//!
//! The wire carries only Linux paths: the server lives in the distribution,
//! and its disk is the authority. Translation therefore exists only at the few
//! places where a path **enters** from the Windows side — the folder picker,
//! the target of a CSV export — and at the one where a Linux path has to
//! **leave** for the Windows desktop (opening the vault in Explorer).
//!
//! Everything is textual and pure: on Linux a `\\wsl.localhost\…` is just a
//! single component, and that is exactly why these functions work on the
//! string — it makes them testable on any machine.

use std::path::{Path, PathBuf};

/// A Windows path brought back to the Linux world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Translated {
    /// The distribution the path names, when it names one
    /// (`\\wsl.localhost\<distro>\…`). It is up to the caller to check it is
    /// the one the server runs in — opening another distribution's repository
    /// in this server would find an empty folder.
    pub distro: Option<String>,
    pub path: PathBuf,
}

/// Translates a Windows path into a distribution path. `None` for what has no
/// equivalent — a network share, an already-Linux path.
///
/// Two forms translate: `\\wsl.localhost\<d>\…` (and its ancestor
/// `\\wsl$\<d>\…`) to `/…`, and `C:\…` to `/mnt/c/…` — the drvfs mounts WSL
/// sets up by default, slow but real, which is enough for the target of an
/// export.
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
            return None; // `C:relative`: relative to the drive's current directory
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

/// The path as the server will understand it.
///
/// A path already written the Linux way passes through unchanged — that is the
/// case for a vault pointed at `/home/…` by hand — a Windows path is
/// translated, and what has no equivalent yields `None`: a network share is
/// reachable from neither side, and staying silent would give an empty folder
/// without saying why.
pub fn for_server(path: &Path) -> Option<PathBuf> {
    if path.to_string_lossy().starts_with('/') {
        return Some(path.to_path_buf());
    }
    to_linux(path).map(|translated| translated.path)
}

/// Translates a distribution path into a Windows path: `/mnt/c/…` becomes
/// `C:\…` again, everything else goes through the `\\wsl.localhost\<distro>\…`
/// share.
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

/// The part after `\\wsl.localhost\` or `\\wsl$\`, whatever the case —
/// Windows paths have none.
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
        let t = to_linux(Path::new(r"\\wsl.localhost\Ubuntu\home\zoé\projects")).unwrap();
        assert_eq!(t.distro.as_deref(), Some("Ubuntu"));
        assert_eq!(t.path, PathBuf::from("/home/zoé/projects"));

        // The older form, and Explorer's casing.
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
        assert_eq!(to_linux(Path::new(r"\\server\share\doc")), None);
        assert_eq!(to_linux(Path::new("/home/already/linux")), None);
        assert_eq!(to_linux(Path::new("C:relative")), None);
    }

    #[test]
    fn the_way_back_mirrors_the_way_in() {
        assert_eq!(
            to_windows(Path::new("/home/zoé/vault"), "Ubuntu"),
            PathBuf::from(r"\\wsl.localhost\Ubuntu\home\zoé\vault")
        );
        assert_eq!(
            to_windows(Path::new("/mnt/c/Users/Arno"), "Ubuntu"),
            PathBuf::from(r"C:\Users\Arno")
        );
        // The round trip of a vault path: what the picker returns must give
        // back what the wire carries.
        let round = to_linux(&to_windows(Path::new("/var/www"), "Ubuntu")).unwrap();
        assert_eq!(round.path, PathBuf::from("/var/www"));
        assert_eq!(round.distro.as_deref(), Some("Ubuntu"));
    }
}
