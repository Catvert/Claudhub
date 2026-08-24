//! What the executable carries that the source tree cannot: the headless
//! server, and — on Windows — the icon and the version metadata.
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
//!
//! The Windows resource is the other half of `tools/claudhub.iss`: the
//! installer's shortcuts point at the executable and take their icon from it,
//! so an executable with no icon resource gives a desktop with a blank one —
//! and the taskbar and the Explorer with it. `assets/claudhub.ico` is
//! versioned rather than built here: the Windows runner has no ImageMagick,
//! and `tools/make_icon.sh` regenerates it when the logo changes.

use std::path::PathBuf;

fn main() {
    embed_server();
    #[cfg(windows)]
    windows_resource();
}

/// The icon and the "Details" tab of the executable's properties.
///
/// `cfg(windows)` and not `CARGO_CFG_TARGET_OS`: it is the *host* that Cargo
/// matches the build-dependency against, and the two must agree — the crate is
/// simply absent everywhere else. Nothing cross-compiles to Windows here; the
/// release leg runs on a Windows runner.
#[cfg(windows)]
fn windows_resource() {
    println!("cargo:rerun-if-changed=assets/claudhub.ico");
    let mut res = winresource::WindowsResource::new();
    // Relative to the manifest directory, which is the build script's cwd.
    res.set_icon("assets/claudhub.ico");
    // `FileVersion` and `ProductVersion` come from `CARGO_PKG_VERSION` on their
    // own; these are the fields Windows shows and which would otherwise be
    // blank — including in "Apps & features", where the installer's entry
    // borrows them.
    res.set("ProductName", "Claudhub");
    res.set("FileDescription", "Claudhub");
    res.set("LegalCopyright", "Apache-2.0");
    // A resource that will not compile must stop the build: shipping an
    // installer whose shortcuts have no icon is exactly what this exists to
    // prevent, and it is invisible on the machine that builds it.
    res.compile().expect("compiling the Windows resource");
}

/// Embeds the headless server into the interface executable.
fn embed_server() {
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
