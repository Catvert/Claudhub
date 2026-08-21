//! Embeds the headless server into the interface executable.
//!
//! On Windows the workers run inside a WSL2 distribution, which needs a binary
//! of its own. Shipping it *beside* the executable worked, but left two files
//! in the archive — one of which nobody knew what to do with — and nothing
//! stopped an old server from sitting next to a fresh interface.
//!
//! `CLAUDHUB_EMBED_SERVER` names the binary to embed; CI sets it once the musl
//! target is built. Without it — the case for every development build, which
//! has cross-compiled nothing — the constant is `None` and
//! `wsl::ensure_installed` falls back to the neighbouring file.
//!
//! The path is written with `{:?}`, which yields an escaped Rust literal: on
//! Windows it contains backslashes, and copying them verbatim would put escape
//! sequences in the middle of the path.

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
