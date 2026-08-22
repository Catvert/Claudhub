//! What a plugin is allowed to do to the outside world.
//!
//! **These are data, and they carry no Rune.** A capability is described here,
//! travels in a `Cmd` and is executed by a worker — possibly a worker in
//! another process, since that is what the WSL server is. That is the whole
//! reason this module sits in the core and the script host does not: the
//! headless server must be able to run a plugin's requests without carrying a
//! scripting engine, and `just check-server` is what proves it still can.
//!
//! **The list is closed, and closing it is the point.** postcard is
//! positional and `PROTOCOL_VERSION` is announced at the handshake: a plugin
//! that could add a message would break the wire for every plugin. Adding a
//! capability is therefore a change to Claudhub, versioned once; adding a
//! plugin is not a change to the wire at all.

use std::path::PathBuf;
use std::time::Duration;

use crate::runtime::Secret;

/// A request's ceiling. The same order as Sentry's, and for the same reason: a
/// remote API sometimes takes seconds, and never minutes.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// A command's ceiling. Longer than HTTP's: what a plugin shells out to is
/// often a CLI that itself talks to a network (`gh`, `docker`).
const SHELL_TIMEOUT: Duration = Duration::from_secs(60);

/// The placeholder a header may carry in place of the secret.
///
/// The secret travels beside the request in a [`Secret`], whose `Debug` masks
/// it, and is substituted here — that is, in the worker. Writing it into the
/// header string on the script's side would put it back into something a
/// `Debug` prints.
const SECRET: &str = "{secret}";

/// One thing a plugin asks of the outside world.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum Cap {
    /// An HTTP request. **Network queue**: this is Sentry's profile, seconds
    /// rather than milliseconds, and it must not occupy a read worker.
    Http {
        method: String,
        url: String,
        /// A value may contain `{secret}`, replaced by `secret` here.
        headers: Vec<(String, String)>,
        body: Option<String>,
        secret: Option<Secret>,
    },
    /// A shell command, run in a worktree. **Background queue**: it is the
    /// same profile as the `wt` status sweep — useful, periodic, and never
    /// worth putting in front of a diff that has just been asked for.
    Shell { worktree: PathBuf, command: String },
}

impl Cap {
    /// The name for the journal, on the model of `Cmd::name`: the variant
    /// without its payload, an HTTP body being no more printable than a file.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Http { .. } => "Http",
            Self::Shell { .. } => "Shell",
        }
    }

    /// Does it belong to the network queue rather than the background one.
    ///
    /// A function of the capability alone, so the routing stays one readable
    /// table — the rule the five queues already live by.
    pub fn is_network(&self) -> bool {
        matches!(self, Self::Http { .. })
    }

    /// Runs it. **In a worker**, never in the interface thread.
    ///
    /// The error is already a `String` and not an `anyhow::Error`: it goes into
    /// an `Evt`, which is `Clone`, and the script only ever shows one sentence
    /// of it.
    pub fn run(self) -> Result<String, String> {
        match self {
            Self::Http {
                method,
                url,
                headers,
                body,
                secret,
            } => http(&method, &url, &headers, body.as_deref(), secret.as_ref()),
            Self::Shell { worktree, command } => shell(&worktree, &command),
        }
    }
}

fn http(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: Option<&str>,
    secret: Option<&Secret>,
) -> Result<String, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .build()
        .into();
    let headers = resolve(headers, secret)?;

    // Four arms and not one loop over a generic builder: ureq 3 types a request
    // by whether it carries a body, and the two builders are different types.
    // The duplication is the price of that guarantee, and it is three lines.
    macro_rules! with_headers {
        ($request:expr) => {{
            let mut request = $request;
            for (name, value) in &headers {
                request = request.header(name, value);
            }
            request
        }};
    }
    let sent = match method.to_uppercase().as_str() {
        "GET" => with_headers!(agent.get(url)).call(),
        "DELETE" => with_headers!(agent.delete(url)).call(),
        "POST" => with_headers!(agent.post(url)).send(body.unwrap_or_default()),
        "PUT" => with_headers!(agent.put(url)).send(body.unwrap_or_default()),
        other => return Err(format!("unsupported HTTP method: {other}")),
    };

    let mut response = sent.map_err(|e| format!("{url}: {e}"))?;
    let status = response.status();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("unreadable answer from {url}: {e}"))?;
    if !status.is_success() {
        // The body is kept: an API says *why* it refused in there, and a bare
        // "answered 422" is exactly the message one comes back to read twice.
        return Err(format!("{url} answered {status} — {}", first_line(&text)));
    }
    Ok(text)
}

/// Puts the secret into the headers that asked for it.
fn resolve(
    headers: &[(String, String)],
    secret: Option<&Secret>,
) -> Result<Vec<(String, String)>, String> {
    headers
        .iter()
        .map(|(name, value)| match secret {
            Some(secret) => Ok((name.clone(), value.replace(SECRET, &secret.0))),
            // A header still asking for a secret that was never given would go
            // out with the placeholder in it, which reads as a mysterious 401.
            None if value.contains(SECRET) => Err(format!("no secret for the header {name}")),
            None => Ok((name.clone(), value.clone())),
        })
        .collect()
}

fn shell(worktree: &std::path::Path, command: &str) -> Result<String, String> {
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(worktree)
        // The same guard as every git command's: with stdin open, a program
        // asking for a password holds a worker forever.
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let output = crate::git::wait_with_timeout(cmd, SHELL_TIMEOUT, || format!("sh -c {command}"))
        .map_err(|e| format!("{e:#}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "{command} failed ({}) — {}",
            output.status,
            first_line(&stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// What a message leads with. A CLI writes a paragraph; a panel shows a line.
fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_goes_to_the_network_and_a_shell_does_not() {
        // The routing is what keeps a plugin from ever holding up a diff. It is
        // read off the capability alone, like `queue_of` off a command.
        assert!(Cap::Http {
            method: "GET".into(),
            url: "https://example.test".into(),
            headers: Vec::new(),
            body: None,
            secret: None,
        }
        .is_network());
        assert!(!Cap::Shell {
            worktree: PathBuf::from("/p/site"),
            command: "gh run list".into(),
        }
        .is_network());
    }

    /// A header still holding the placeholder would go out as it stands, and
    /// the answer would be a 401 nothing explains.
    #[test]
    fn a_header_asking_for_a_missing_secret_is_refused() {
        let result = Cap::Http {
            method: "GET".into(),
            url: "https://example.invalid".into(),
            headers: vec![("Authorization".into(), "Bearer {secret}".into())],
            body: None,
            secret: None,
        }
        .run();
        let message = result.expect_err("no secret, no request");
        assert!(message.contains("Authorization"), "{message}");
    }

    /// A CLI writes a paragraph on failure; a panel row shows a line.
    #[test]
    fn a_command_that_fails_says_so_in_one_line() {
        let result = Cap::Shell {
            worktree: PathBuf::from("."),
            command: "seq 3 >&2; exit 9".into(),
        }
        .run();
        let message = result.expect_err("exit 9 is a failure");
        let reported = message.rsplit("— ").next().expect("a reason");
        assert_eq!(reported, "1", "{message}");
        assert!(message.contains("exit status: 9"), "{message}");
    }

    #[test]
    fn a_command_that_works_gives_its_output_back() {
        let out = Cap::Shell {
            worktree: PathBuf::from("."),
            command: "echo bonjour".into(),
        }
        .run()
        .expect("echo works");
        assert_eq!(out.trim(), "bonjour");
    }
}
