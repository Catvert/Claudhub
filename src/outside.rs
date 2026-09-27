//! What Claudhub asks of the world outside the repository.
//!
//! Two views read a service rather than the checkout — Sentry over HTTP, the
//! CI runs through `gh` — and this is the whole of what they are allowed to do
//! with it. **These are data**: a request is described here, travels in a
//! `Cmd` and is executed by a worker, possibly a worker in another process,
//! since that is what the WSL server is. `just check-server` is what proves the
//! headless one still carries it.
//!
//! It outlived the scripting layer it was written for. That layer put a Rune
//! script in front of these two requests and a vocabulary to paint a panel
//! behind them; the two views it ever carried are Rust now, and what stayed is
//! the half that was never about scripting — one closed list of things one may
//! do outside, each with the queue it belongs in.
//!
//! **The list is closed, and closing it is the point.** postcard is positional
//! and `PROTOCOL_VERSION` is announced at the handshake: what crosses the wire
//! is versioned once, here, rather than growing a message per feature.

use std::path::PathBuf;
use std::time::Duration;

use crate::runtime::Secret;

/// A request's ceiling. The same order as Sentry's, and for the same reason: a
/// remote API sometimes takes seconds, and never minutes.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// A command's ceiling. Longer than HTTP's: what is shelled out to is a CLI
/// that itself talks to a network — `gh`, which is what the CI view runs.
const SHELL_TIMEOUT: Duration = Duration::from_secs(60);

/// The placeholder a header may carry in place of the secret.
///
/// The secret travels beside the request in a [`Secret`], whose `Debug` masks
/// it, and is substituted here — that is, in the worker. Writing it into the
/// header string on the script's side would put it back into something a
/// `Debug` prints.
const SECRET: &str = "{secret}";

/// One thing Claudhub asks of the outside world.
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
    /// worth putting in front of a diff that has just been asked for. It is
    /// what the CI view runs `gh` with.
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
    /// A function of the request alone, so the routing stays one readable
    /// table — the rule the seven queues already live by.
    pub fn is_network(&self) -> bool {
        matches!(self, Self::Http { .. })
    }

    /// Runs it. **In a worker**, never in the interface thread.
    ///
    /// The error is already a `String` and not an `anyhow::Error`: it goes into
    /// an `Evt`, which is `Clone`, and a panel only ever shows one sentence of
    /// it.
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
    let agent = agent();
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

    // The URL and never the headers: the token travels in the latter, and an
    // error message ends up in a bubble and in the log.
    let mut response = sent.map_err(|e| format!("{url}: {e}"))?;
    let status = response.status();
    let text = response.body_mut().read_to_string();
    if !status.is_success() {
        // The body is kept: an API says *why* it refused in there, and a bare
        // "answered 422" is exactly the message one comes back to read twice.
        // A body that does not read still leaves the status, which is the half
        // of the answer that matters.
        let why = text.as_deref().map(first_line).unwrap_or_default();
        return Err(format!("{url} answered {status} — {why}"));
    }
    text.map_err(|e| format!("unreadable answer from {url}: {e}"))
}

/// One agent for the whole process: it holds the connection pool and the TLS
/// setup, and building one per call threw away the session a view polling the
/// same host every ten seconds would have reused.
///
/// **A status is not an error here.** ureq 3 turns every 4xx and 5xx into an
/// `Error::StatusCode` by default, which carries the number and drops the body
/// — so the branch of `http` that reads *why* an API refused was never reached,
/// and a 403 said "403" and nothing of the scope the token was missing.
fn agent() -> &'static ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(HTTP_TIMEOUT))
            .http_status_as_error(false)
            .build()
            .into()
    })
}

/// Puts the secret into the headers that asked for it.
fn resolve(
    headers: &[(String, String)],
    secret: Option<&Secret>,
) -> Result<Vec<(String, String)>, String> {
    let secret = secret.map(|secret| Secret(from_env(&secret.0)));
    let secret = secret.as_ref();
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

/// The command goes out in English: its lines are Claudhub's own `gh`
/// invocations, and what they say on failure is quoted in a panel.
fn shell(worktree: &std::path::Path, command: &str) -> Result<String, String> {
    let mut cmd = sh(command, None);
    cmd.current_dir(worktree);
    let output = Bounded::new(cmd, format!("sh -c {command}"), SHELL_TIMEOUT)
        .in_english()
        .output()
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

// — Running a program ————————————————————————————————————————————————

/// A program a worker runs to its end, with every guard a worker's subprocess
/// needs — and one place to put them. Five callers used to write them out,
/// and each had forgotten a different one.
///
/// - **stdin closed**, unless something is fed to it: a program deciding to
///   ask a question — a password, a bootstrap reading its input — would hold
///   the worker for good.
/// - **Both outputs piped and drained while it runs**, and a **ceiling**:
///   `git::wait_feeding`, which also stops waiting on a daemon that inherited
///   the pipes.
/// - **No console window** on Windows (`wsl::no_console`): the interface is a
///   windowed program, and each console child would flash one.
///
/// `LC_ALL=C` is **not** among them, and on purpose: it is asked for
/// ([`Bounded::in_english`]) by a caller running a tool whose complaint it
/// quotes — `just`, a test runner, `gh`. A project's own shell line or the
/// user's agent writes text the user reads, which the C locale could turn to
/// question marks or another language.
pub(crate) struct Bounded {
    cmd: std::process::Command,
    what: String,
    limit: Duration,
    input: Option<Vec<u8>>,
}

impl Bounded {
    /// `what` names the program in the errors — which one did not answer,
    /// which one failed.
    pub(crate) fn new(
        cmd: std::process::Command,
        what: impl Into<String>,
        limit: Duration,
    ) -> Self {
        Self {
            cmd,
            what: what.into(),
            limit,
            input: None,
        }
    }

    /// Error messages read in English, for the reason the git layer reads
    /// them there: they are quoted, and matched on.
    pub(crate) fn in_english(mut self) -> Self {
        self.cmd.env("LC_ALL", "C");
        self
    }

    /// Something to write on its standard input, which is then open.
    pub(crate) fn feeding(mut self, input: Vec<u8>) -> Self {
        self.input = Some(input);
        self
    }

    /// Runs it and gives back all it said, success or not.
    pub(crate) fn output(self) -> anyhow::Result<std::process::Output> {
        use std::process::Stdio;
        let Self {
            mut cmd,
            what,
            limit,
            input,
        } = self;
        cmd.stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        crate::wsl::no_console(&mut cmd);
        crate::git::wait_feeding(cmd, input, limit, || what.clone())
    }

    /// Runs it and gives back its standard output — or, when it failed, an
    /// error saying so with what it wrote on stderr.
    pub(crate) fn stdout(self) -> anyhow::Result<String> {
        let what = self.what.clone();
        let out = self.output()?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let said = stderr.trim();
            anyhow::bail!(
                "{what} failed ({}): {}",
                out.status,
                if said.is_empty() { "no message" } else { said }
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// `sh -c <line>`: a line written for a POSIX shell, whatever the user's login
/// shell is. `shell` names another one to read it — `wt`'s `WT_SHELL`.
pub(crate) fn sh(line: &str, shell: Option<&str>) -> std::process::Command {
    let mut cmd = std::process::Command::new(shell.unwrap_or("sh"));
    cmd.arg("-c").arg(line);
    cmd
}

/// A secret written `$NAME` is read from the environment, **here**.
///
/// Here, that is, in the worker: the server's environment is what counts, which
/// is the rule the Sentry token has always lived by. It is also what lets a
/// token stay out of a settings file that gets copied around, and it costs the
/// caller nothing — the value it names is opaque to it either way. A variable
/// that is not set leaves the text as it stands, so a secret that genuinely
/// begins with a dollar still works.
fn from_env(value: &str) -> String {
    let Some(name) = value.strip_prefix('$') else {
        return value.to_string();
    };
    std::env::var(name).unwrap_or_else(|_| value.to_string())
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
        // The routing is what keeps a slow service from ever holding up a
        // diff. It is read off the request alone, like `queue_of` off a
        // command.
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

    /// The body of a refusal is what says why, and an agent that turns the
    /// status into an error throws it away before `http` can read it.
    #[test]
    fn a_refusal_is_read_rather_than_raised() {
        assert!(!agent().config().http_status_as_error());
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

    /// A token in the environment rather than in a file one copies around.
    #[test]
    fn a_secret_can_name_a_variable() {
        // Safety: the test process, one variable, read back at once.
        unsafe { std::env::set_var("CLAUDHUB_TEST_TOKEN", "s3cret") };
        let resolved = resolve(
            &[("Authorization".into(), "Bearer {secret}".into())],
            Some(&Secret("$CLAUDHUB_TEST_TOKEN".into())),
        )
        .expect("the variable is set");
        assert_eq!(resolved[0].1, "Bearer s3cret");
        // A variable that is not set leaves the text as it stands: a secret
        // that genuinely begins with a dollar still works.
        let plain = resolve(
            &[("Authorization".into(), "Bearer {secret}".into())],
            Some(&Secret("$nothing-is-set-here".into())),
        )
        .expect("no variable, no failure");
        assert_eq!(plain[0].1, "Bearer $nothing-is-set-here");
    }

    /// A program reading its input finds it closed and goes on, rather than
    /// holding the worker until the ceiling.
    #[test]
    fn a_program_reading_its_input_finds_it_closed() {
        let out = Bounded::new(sh("cat", None), "cat", Duration::from_secs(5))
            .stdout()
            .expect("cat reads nothing and ends");
        assert_eq!(out, "");
    }

    #[test]
    fn what_is_fed_reaches_the_program() {
        let out = Bounded::new(sh("tr a-z A-Z", None), "tr", Duration::from_secs(5))
            .feeding(b"bonjour".to_vec())
            .stdout()
            .expect("tr works");
        assert_eq!(out, "BONJOUR");
    }

    /// The failure names the program, its exit and what it said.
    #[test]
    fn a_failure_says_who_and_why() {
        let error = Bounded::new(
            sh("echo nope >&2; exit 3", None),
            "probe",
            Duration::from_secs(5),
        )
        .stdout()
        .expect_err("exit 3 is a failure");
        assert_eq!(format!("{error:#}"), "probe failed (exit status: 3): nope");
    }

    /// English is asked for, never imposed: a project's own line prints text
    /// the user reads.
    #[test]
    fn english_is_asked_for_and_not_imposed() {
        let locale = |bounded: Bounded| bounded.stdout().expect("echo works");
        let line = || sh("echo \"${LC_ALL-unset}\"", None);
        assert_eq!(
            locale(Bounded::new(line(), "echo", Duration::from_secs(5)).in_english()).trim(),
            "C"
        );
        let mut untouched = line();
        untouched.env_remove("LC_ALL");
        assert_eq!(
            locale(Bounded::new(untouched, "echo", Duration::from_secs(5))).trim(),
            "unset"
        );
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
