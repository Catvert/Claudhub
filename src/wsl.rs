//! The server inside the distribution: finding it, installing it, launching it.
//!
//! This is the Windows half of the split. The interface is a native `.exe`, the
//! workers run in WSL2, and between the two the server binary has to *be* in
//! the distribution — the user having no reason to put it there by hand. It is
//! therefore shipped beside the executable and copied on first opening, the way
//! VS Code does it.
//!
//! **The installation is content-addressed**, never by version number: the
//! digest of the shipped binary names its folder. Two different `.exe`s
//! therefore install two different servers without treading on each other, an
//! update installs itself, and a development build — which has no number —
//! behaves like the rest. It is the same pattern as `tools/make_appimage.sh`,
//! for the same reason.
//!
//! **Nothing here goes through a login shell.** `wsl.exe --exec` launches the
//! program directly: what is written there has to be an absolute path, not a
//! `~` nobody would expand. Hence [`probe`], which asks the distribution once
//! and for all where the user's home is and which shell belongs to them.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

/// The binary's name, the same on both sides of the wire.
pub const SERVER_BIN: &str = "claudhub-server";

// `EMBEDDED`: the server embedded in the executable, when there is one.
// Set by `build.rs` from `CLAUDHUB_EMBED_SERVER` — present in what CI ships,
// absent from a development build, which has no musl binary to hand. See
// `bundled_server` for the fallback.
include!(concat!(env!("OUT_DIR"), "/server.rs"));

/// What the distribution tells us about itself, in a single round trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// The user's home, absolute: `--exec` does not expand `~`.
    pub home: String,
    /// Their login shell, as `/etc/passwd` declares it.
    ///
    /// It is asked for here because this is the only place we can: a terminal
    /// launched by `--exec` has no shell to query `$SHELL` from, and a shell is
    /// precisely what we want to launch.
    pub shell: String,
}

/// The installed distributions, in the order `wsl.exe` gives them.
///
/// The first is the default one, which makes it a reasonable suggestion when
/// one has to be chosen.
pub fn distributions() -> Result<Vec<String>> {
    let out = wsl()
        .args(["--list", "--quiet"])
        .output()
        .context("wsl.exe not found: is WSL installed?")?;
    let text = decode(&out.stdout);
    let list: Vec<String> = text
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();
    if list.is_empty() {
        bail!("no WSL distribution installed");
    }
    Ok(list)
}

/// Asks the distribution where the user lives and which shell belongs to them.
pub fn probe(distro: &str) -> Result<Probe> {
    // `getent` rather than `$SHELL`: the variable is only set by a login shell,
    // and there is none here. Field 7 of `passwd` is authoritative.
    let out = run(
        distro,
        "printf %s\\\\n $HOME; getent passwd $(id -u) | cut -d: -f7",
    )?;
    let mut lines = out.lines();
    let home = lines.next().unwrap_or_default().trim().to_string();
    let shell = lines.next().unwrap_or_default().trim().to_string();
    if home.is_empty() {
        bail!("the distribution \"{distro}\" did not say where the user's home is");
    }
    Ok(Probe {
        home,
        // A home without a declared shell is unlikely, but `sh` exists everywhere.
        shell: if shell.is_empty() {
            "/bin/sh".into()
        } else {
            shell
        },
    })
}

/// The server binary shipped beside the executable.
///
/// The development build's fallback: what is shipped carries its server inside
/// (see `EMBEDDED`), but a local build has no musl binary to embed, and putting
/// the file next to it is still the way to give oneself one.
pub fn bundled_server() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("executable path not found")?;
    let path = exe
        .parent()
        .map(|dir| dir.join(SERVER_BIN))
        .unwrap_or_default();
    if !path.is_file() {
        bail!(
            "this executable carries no embedded server and {SERVER_BIN} \
             is missing from beside it ({}): build the musl target and put \
             the binary there, or go through CLAUDHUB_SERVER_CMD",
            path.display()
        );
    }
    Ok(path)
}

/// The bytes of the server to install: the executable's if it carries any, the
/// neighbouring file's otherwise.
fn server_bytes() -> Result<Vec<u8>> {
    if let Some(bytes) = EMBEDDED {
        return Ok(bytes.to_vec());
    }
    let source = bundled_server()?;
    std::fs::read(&source).with_context(|| format!("cannot read {}", source.display()))
}

/// Installs the server into the distribution if it is not already there, and
/// returns its absolute path.
///
/// Granting the execute bit is not a precaution: a zip archive does not carry
/// it, and it is the failure everybody runs into when copying the binary by
/// hand.
pub fn ensure_installed(distro: &str, probe: &Probe) -> Result<String> {
    let bytes = server_bytes()?;
    let id = content_id(&bytes);
    let dir = format!("{}/.claudhub/bin/{id}", probe.home);
    let target = format!("{dir}/{SERVER_BIN}");

    if run(distro, &format!("test -x {target} && echo ok")).is_ok_and(|out| out.trim() == "ok") {
        return Ok(target);
    }

    // Written alongside then renamed: a `mv` is atomic, so an interrupted
    // installation does not leave a truncated binary that the next launch would
    // take for a good one.
    let script = install_script(&dir, &target, &probe.home, &id);
    let mut child = wsl()
        .args(["-d", distro, "--exec", "/bin/sh", "-c", &script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("wsl.exe could not launch the installation")?;
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().expect("stdin requested");
        stdin
            .write_all(&bytes)
            .context("sending the server into the distribution")?;
    }
    let out = child.wait_with_output().context("installing the server")?;
    if !out.status.success() {
        bail!(
            "installing the server into \"{distro}\": {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(target)
}

/// The install script, written without a single quote.
///
/// This is deliberate: the line crosses `CreateProcess`, then `wsl.exe`, which
/// rebuilds the `argv` its own way — every quote there is a chance to get
/// eaten. The price is that a home containing a space would not work; none has
/// ever been seen on Linux.
///
/// The purge keeps the current build's folder and throws the others away:
/// without it, every update would leave twelve megabytes behind.
fn install_script(dir: &str, target: &str, home: &str, id: &str) -> String {
    format!(
        "set -e; mkdir -p {dir}; cat > {target}.part; chmod +x {target}.part; \
         mv {target}.part {target}; \
         find {home}/.claudhub/bin -mindepth 1 -maxdepth 1 ! -name {id} -exec rm -rf {{}} +"
    )
}

/// The command line that launches the server inside the distribution.
///
/// `--cd` is not a convenience: the server announces its start directory in its
/// handshake, and that is what opens the repository one came from — local
/// mode's "launched from its project".
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

/// A terminal tab's command line, through `wsl.exe`.
///
/// On Windows the repositories live in the distribution: a terminal opening
/// locally would look at a path that does not exist, and the agent launched in
/// it would not see the code. The pty stays local — ConPTY carries it — and the
/// emulation does not change by a byte.
///
/// The environment goes through `/usr/bin/env` and not through the Windows
/// process's variables: what counts is what the **Linux** process sees, and
/// `wsl.exe` does not forward its caller's environment. `--exec` prevents an
/// intermediate shell from re-splitting the arguments.
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
        // The login shell, and **as a login shell** (`-l`): it is what every
        // terminal does, and it is what reads `.profile` — otherwise the user's
        // `PATH` would miss half their tools, often including the agent they
        // want to launch.
        None => {
            args.push(login_shell.to_string());
            args.push("-l".into());
        }
    }
    ("wsl.exe".to_string(), args)
}

/// A content's digest, in hexadecimal — FNV-1a 64-bit, like the one for open
/// files, and for the same reason: it has to be the same from one binary to the
/// next.
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

/// Runs a shell command inside the distribution and returns its standard output.
fn run(distro: &str, script: &str) -> Result<String> {
    let out = wsl()
        .args(["-d", distro, "--exec", "/bin/sh", "-c", script])
        .output()
        .context("wsl.exe not found: is WSL installed?")?;
    if !out.status.success() {
        bail!(
            "\"{distro}\" refused: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(decode(&out.stdout))
}

fn wsl() -> Command {
    let mut cmd = Command::new("wsl.exe");
    // Since WSL 0.64 this variable makes `wsl.exe` output UTF-8; without it,
    // `--list` answers in UTF-16, hence `decode`'s fallback.
    cmd.env("WSL_UTF8", "1");
    cmd
}

/// Decodes what `wsl.exe` writes, in UTF-8 as in UTF-16.
///
/// Versions from before `WSL_UTF8` answer in little-endian UTF-16, which, read
/// as UTF-8, gives a two-character name with a null byte between each letter —
/// a perfectly unreadable list of distributions.
fn decode(bytes: &[u8]) -> String {
    let body = bytes.strip_prefix(&[0xFF, 0xFE]).unwrap_or(bytes);
    let nuls = body.iter().filter(|b| **b == 0).count();
    if body.len() >= 2 && nuls * 4 >= body.len() {
        let units: Vec<u16> = body
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(body).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What we embed must be the Linux binary, not the Windows executable nor a
    /// trace file picked up along the way: a path mistake in CI would only show
    /// on the first startup on a user's machine, and would read as a broken
    /// distribution.
    #[test]
    fn an_embedded_server_is_a_linux_binary() {
        let Some(bytes) = EMBEDDED else {
            return; // development build: nothing is embedded
        };
        assert_eq!(&bytes[..4], b"\x7fELF", "the embedded server is not an ELF");
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
        // An accented name must not be lost in the conversion.
        let accented: Vec<u8> = "Ubuntu-Café\n"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        assert_eq!(decode(&accented), "Ubuntu-Café\n");
    }

    /// Identical content gives the same digest, different content another one:
    /// that is all content addressing asks for.
    #[test]
    fn the_content_names_the_install() {
        assert_eq!(content_id(b"a server"), content_id(b"a server"));
        assert_ne!(content_id(b"a server"), content_id(b"another"));
        assert_eq!(content_id(b"").len(), 16);
    }

    /// The script must contain no quotes: they do not survive the trip through
    /// `wsl.exe`, and it is the failure you only understand after living
    /// through it.
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
        // The atomic rename, and the purge of what is no longer the current
        // build.
        assert!(script.contains(".part"), "{script}");
        assert!(script.contains("! -name ff"), "{script}");
    }

    /// An ordinary tab opens the login shell in the worktree, with what the
    /// agent needs to know in its environment.
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

    /// An explicit command — an agent, a `wt` task — passes through whole,
    /// arguments included: it is what we asked to launch.
    #[test]
    fn an_explicit_command_keeps_its_arguments() {
        let command = Some((
            "sh".to_string(),
            vec!["-lc".to_string(), "composer install && exit".to_string()],
        ));
        let (_, args) = terminal_argv("Ubuntu", "/home/a/p", "/bin/sh", command, &[]);
        // The composed argument stays **one** element: re-splitting it would
        // run it crooked.
        assert_eq!(args.last().unwrap(), "composer install && exit");
        assert_eq!(args[args.len() - 3], "sh");
        assert!(!args.contains(&"-l".to_string()));
    }

    #[test]
    fn the_launch_line_carries_the_working_directory_when_there_is_one() {
        assert_eq!(
            launch_argv("Ubuntu", "/home/a/s", Some("/home/a/project")),
            vec![
                "wsl.exe",
                "-d",
                "Ubuntu",
                "--cd",
                "/home/a/project",
                "--exec",
                "/home/a/s"
            ]
        );
        assert_eq!(
            launch_argv("Ubuntu", "/home/a/s", None),
            vec!["wsl.exe", "-d", "Ubuntu", "--exec", "/home/a/s"]
        );
        // An empty string is not a directory: it would make a `--cd` with no
        // argument, and `wsl.exe` would swallow `--exec` in its place.
        assert_eq!(
            launch_argv("Ubuntu", "/home/a/s", Some("")),
            vec!["wsl.exe", "-d", "Ubuntu", "--exec", "/home/a/s"]
        );
    }
}
