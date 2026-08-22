//! Paths in and out of `file://` URIs.
//!
//! LSP names documents by URI, and every answer a server gives — a definition,
//! a diagnostic — comes back as one. Twenty lines here rather than a URL crate:
//! we build one shape and read one shape, both of them absolute local paths,
//! and a general parser would bring hosts, queries and fragments we never
//! write.
//!
//! Only Linux paths cross this module. That is not a simplification but the
//! wire's rule: the workers run where the files are — inside the WSL
//! distribution when the interface is a Windows `.exe` — and it is the server
//! side that builds these URIs. The translation to a Windows path, when there
//! has to be one, happens at the four edges `wslpath` already covers.

use std::path::{Path, PathBuf};

/// The URI naming a file. Everything outside the unreserved set is
/// percent-encoded, the separators excepted: a space in a path is common
/// enough, and a server handed a raw one answers about a document it could not
/// parse rather than saying so.
pub fn of(path: &Path) -> String {
    let mut uri = String::from("file://");
    for byte in path.to_string_lossy().as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                uri.push(*byte as char)
            }
            other => uri.push_str(&format!("%{other:02X}")),
        }
    }
    uri
}

/// The path a URI names, or `None` when it names something else — a server may
/// answer about `untitled:` or a scheme of its own, and following that as if it
/// were a file would open a path made of its scheme.
pub fn path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // `file:///home/…` has an empty authority; `file://host/…` has one, and the
    // path is what starts at the first slash either way.
    let start = rest.find('/')?;
    let decoded = decode(&rest[start..])?;
    Some(PathBuf::from(decoded))
}

fn decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_path_reads_back() {
        let path = Path::new("/home/finch/Projets/app/src/User.php");
        assert_eq!(of(path), "file:///home/finch/Projets/app/src/User.php");
        assert_eq!(super::path(&of(path)).unwrap(), path);
    }

    /// A space and an accent are the two that actually occur, and both must
    /// survive the round trip.
    #[test]
    fn spaces_and_accents_are_encoded_and_decoded() {
        let path = Path::new("/home/finch/Mes Projets/Modèle.php");
        let uri = of(path);
        assert_eq!(uri, "file:///home/finch/Mes%20Projets/Mod%C3%A8le.php");
        assert_eq!(super::path(&uri).unwrap(), path);
    }

    /// What a server answers is not always a file, and treating another scheme
    /// as one would open a path made of its own name.
    #[test]
    fn another_scheme_is_not_a_path() {
        assert!(super::path("untitled:Untitled-1").is_none());
        assert!(super::path("https://example.org/x").is_none());
    }

    /// Some servers keep the authority slot filled; the path is what starts at
    /// the first slash regardless.
    #[test]
    fn an_authority_is_skipped() {
        assert_eq!(
            super::path("file://localhost/etc/hosts").unwrap(),
            Path::new("/etc/hosts")
        );
    }
}
