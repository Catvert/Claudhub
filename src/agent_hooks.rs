//! What an agent says of itself: Claude Code's hooks, and the files they leave.
//!
//! `agent::Tracker` **guesses** whether an agent works from the processor it
//! burns, and a guess cannot say the two things one watches five agents for:
//! *it has finished*, and *it is waiting for me*. Claude Code can — it runs a
//! command of ours on `Stop`, on a permission prompt, on every prompt sent —
//! and this module is both ends of that channel.
//!
//! **The channel is a file per session**, in `~/.claudhub/agents/`, which the
//! agent sweep reads every two seconds. The hook writes; nothing here watches:
//! a directory we both read and prune would be a watcher woken by its own
//! hand. The folder is the home's and not the repository's for the WSL target
//! — the agents and the workers are in the distribution there, the window is
//! not, and `$HOME` is the one place the hook and the worker agree on without
//! being told.
//!
//! **The hook is a line of POSIX shell, not our binary.** Claudhub is not
//! installed where an agent runs: under WSL the worker is a content-addressed
//! file in `~/.claudhub/bin` that changes with every version — a hook naming it
//! would break at the first update, silently — and under Linux it lives in a
//! Nix store path or an AppImage mount that is gone once the window closes.
//! The line needs `cat`, `mkdir` and `mv`, and parses nothing but the session
//! id, with parameter expansions: the JSON itself is read here, in Rust.
//!
//! No gpui here: the core's tests cover it, and the server builds it.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context as _, Result};
use serde_json::{json, Map, Value};

/// What makes a hook entry ours, in the command itself.
///
/// A shell comment at the end of the line: it survives the user editing the
/// file around it, it is what removal looks for, and changing the command in
/// a later version still finds — and replaces — the old one.
pub const MARKER: &str = "claudhub-agent-state";

/// The command every hook runs.
///
/// Read against the three ways it can go wrong:
///
/// - **Exit 2 blocks the prompt** on `UserPromptSubmit`, and dash exits 2 on
///   an expansion error — hence no `${HOME:?}`, and `exit 0` whatever `mkdir`
///   or `mv` said. The standard output is added to the agent's context on
///   `SessionStart` and `UserPromptSubmit`: nothing here writes to it.
/// - **The session id names a file**, so it is refused unless it is only
///   letters, digits, `-` and `_` — a `../` in it would write anywhere.
/// - **A reader must never see half a file**: written beside, then renamed,
///   the temporary name carrying the shell's pid so two hooks of one session
///   running at once do not share it.
///
/// The first line is `$PPID` — the agent, which spawns the hook's shell — and
/// it is what ties a session to a process: see `agent::Tracker`.
pub const HOOK_COMMAND: &str = concat!(
    r#"[ -n "$HOME" ] || exit 0; j=$(cat); "#,
    r#"case $j in *'"session_id"'*) ;; *) exit 0;; esac; "#,
    r#"s=${j#*'"session_id"'}; s=${s#*'"'}; s=${s%%'"'*}; "#,
    r#"case $s in ''|*[!A-Za-z0-9_-]*) exit 0;; esac; "#,
    r#"d="$HOME/.claudhub/agents"; "#,
    r#"mkdir -p "$d" && printf '%s\n%s\n' "$PPID" "$j" >"$d/$s.$$" && mv -f "$d/$s.$$" "$d/$s.json"; "#,
    "exit 0 # claudhub-agent-state"
);

/// The events hooked, with their matcher.
///
/// - `SessionStart` **without** `compact`: an automatic compaction happens in
///   the middle of a turn, and reading it as "a session starts, idle" would
///   put a working agent to rest.
/// - `PostToolUse` is what says a permission was granted: nothing else fires
///   between the prompt and the end of the turn.
/// - `Notification` only for what asks something of the user — the others
///   (`auth_success`, the quota notices) would overwrite the session's file
///   with a word that is not a state.
const EVENTS: &[(&str, Option<&str>)] = &[
    ("SessionStart", Some("startup|resume|clear")),
    ("UserPromptSubmit", None),
    ("PostToolUse", None),
    (
        "Notification",
        Some("permission_prompt|idle_prompt|elicitation_dialog"),
    ),
    ("Stop", None),
    ("SessionEnd", None),
];

/// The file, relative to a worktree.
pub const SETTINGS_FILE: &str = ".claude/settings.local.json";

/// What an agent last said of itself.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Signal {
    /// A session opened, and nothing asked of it yet.
    Ready,
    /// A prompt was sent, or a tool has just run.
    Working,
    /// The turn is over (`Stop`).
    Finished,
    /// It has been waiting for the next prompt for a while (`idle_prompt`) —
    /// a finished turn as far as the badge goes, but not a new one: it comes
    /// a minute after `Stop`, and announcing it would announce the same end
    /// twice.
    Idle,
    /// It asks something: a permission, or an answer to a question.
    Waiting(String),
    /// The session closed.
    Ended,
}

/// One session, as the worker read it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Record {
    pub session: String,
    /// The worktree its directory belongs to — the deepest, as for a process.
    pub worktree: PathBuf,
    /// The process that ran the hook's shell: the agent, as far as the hook
    /// can tell. `None` when the line is missing or unreadable.
    pub pid: Option<u32>,
    pub signal: Signal,
    /// When the file was written, in milliseconds of the worker's clock: two
    /// readings of the same session tell a new `Stop` from the old one by it.
    pub stamp: u64,
    /// How long ago, when it was read. The window's clock and the worker's are
    /// not the same machine's under WSL; an age crosses, an instant would not.
    pub age_ms: u64,
}

/// The longest message a waiting agent carries across: a badge and a balloon
/// show a line, and a permission prompt can quote a whole command.
const MESSAGE_CAP: usize = 240;

/// Reads one file: `$PPID`, then the hook's JSON.
///
/// `None` for what is not a session — a file cut short, a JSON of another
/// shape, an event we do not hook (read as no word rather than as a state).
pub fn parse(text: &str) -> Option<(Option<u32>, String, PathBuf, Signal)> {
    let (pid, json) = text.split_once('\n')?;
    let pid = pid.trim().parse::<u32>().ok().filter(|pid| *pid > 1);
    let value: Value = serde_json::from_str(json.trim()).ok()?;
    let session = value.get("session_id")?.as_str()?.to_string();
    let cwd = PathBuf::from(value.get("cwd")?.as_str()?);
    let signal = signal_of(&value)?;
    Some((pid, session, cwd, signal))
}

/// Which state an event puts a session in.
///
/// Read field by field and not through a structure, for the reason written on
/// `sentry.rs`: a field present at `null` makes a whole structure fail.
pub fn signal_of(value: &Value) -> Option<Signal> {
    let text = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or("");
    Some(match text("hook_event_name") {
        "SessionStart" => match text("source") {
            "compact" => return None,
            _ => Signal::Ready,
        },
        "UserPromptSubmit" | "PostToolUse" | "PostToolUseFailure" => Signal::Working,
        "Stop" => Signal::Finished,
        "SessionEnd" => Signal::Ended,
        "Notification" => match text("notification_type") {
            "idle_prompt" => Signal::Idle,
            "permission_prompt" | "elicitation_dialog" => {
                Signal::Waiting(cap(text("message").trim()))
            }
            _ => return None,
        },
        _ => return None,
    })
}

fn cap(message: &str) -> String {
    let line = message.lines().next().unwrap_or("");
    match line.char_indices().nth(MESSAGE_CAP) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_string(),
    }
}

/// Where the hooks write: `$HOME/.claudhub/agents`.
pub fn folder() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(|| directories::UserDirs::new().map(|dirs| dirs.home_dir().to_path_buf()))?;
    Some(home.join(".claudhub").join("agents"))
}

/// A file nobody has written to for this long is a session long gone.
const FORGOTTEN: Duration = Duration::from_secs(3 * 24 * 3600);
/// A closed session is kept this long — one sweep has to have seen it close.
const CLOSED: Duration = Duration::from_secs(60);

/// The sessions of these worktrees, as the hooks last left them.
///
/// It also **prunes**: a closed session once it has been read, and anything
/// older than three days — a session killed with its terminal says nothing on
/// the way out. A file of a directory no open worktree holds is left alone:
/// it is some other repository's, and that one may be opened tomorrow.
pub fn read(worktrees: &[PathBuf]) -> Vec<Record> {
    let Some(folder) = folder() else {
        return Vec::new();
    };
    read_in(&folder, worktrees, SystemTime::now())
}

fn read_in(folder: &Path, worktrees: &[PathBuf], now: SystemTime) -> Vec<Record> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut records = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) else {
            continue;
        };
        let age = now.duration_since(modified).unwrap_or_default();
        if age > FORGOTTEN {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some((pid, session, cwd, signal)) = parse(&text) else {
            continue;
        };
        if signal == Signal::Ended && age > CLOSED {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        let Some(worktree) = crate::agent::owning_worktree(worktrees, &cwd) else {
            continue;
        };
        let stamp = modified
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|since| since.as_millis() as u64)
            .unwrap_or(0);
        records.push(Record {
            session,
            worktree,
            pid,
            signal,
            stamp,
            age_ms: age.as_millis() as u64,
        });
    }
    records
}

// — The settings file ————————————————————————————————————————————————

fn is_ours(hook: &Value) -> bool {
    hook.get("command")
        .and_then(Value::as_str)
        .is_some_and(|command| command.contains(MARKER))
}

/// Takes our entries out of a settings document, leaving everything else as
/// it was.
///
/// Entry by entry, not group by group: a group that holds one of ours beside
/// a hook of the user's — written by hand around ours — loses ours and keeps
/// theirs. What is left empty goes: an empty group, an empty event, an empty
/// `hooks`.
pub fn without_ours(mut settings: Value) -> Result<Value> {
    let Some(root) = settings.as_object_mut() else {
        bail!("{SETTINGS_FILE} is not a JSON object");
    };
    let Some(hooks) = root.get_mut("hooks") else {
        return Ok(settings);
    };
    let Some(events) = hooks.as_object_mut() else {
        bail!("\"hooks\" in {SETTINGS_FILE} is not an object");
    };
    for groups in events.values_mut() {
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        for group in groups.iter_mut() {
            if let Some(list) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                list.retain(|hook| !is_ours(hook));
            }
        }
        groups.retain(|group| {
            group
                .get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|list| !list.is_empty())
        });
    }
    events.retain(|_, groups| groups.as_array().is_none_or(|groups| !groups.is_empty()));
    if events.is_empty() {
        root.remove("hooks");
    }
    Ok(settings)
}

/// Puts our entries in a settings document — the one on disk, or none.
///
/// **Idempotent**: ours are taken out first, then added once at the end of
/// each event's list, so installing twice writes the same file as once, and a
/// newer command replaces an older one instead of running beside it.
pub fn with_ours(settings: Option<Value>) -> Result<Value> {
    let mut settings = without_ours(settings.unwrap_or_else(|| json!({})))?;
    let root = settings
        .as_object_mut()
        .expect("without_ours checked it is an object");
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .expect("without_ours checked it is an object");
    for (event, matcher) in EVENTS {
        let mut group = Map::new();
        if let Some(matcher) = matcher {
            group.insert("matcher".into(), json!(matcher));
        }
        group.insert(
            "hooks".into(),
            json!([{ "type": "command", "command": HOOK_COMMAND, "timeout": 10 }]),
        );
        let groups = hooks
            .entry(event.to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        match groups.as_array_mut() {
            Some(groups) => groups.push(Value::Object(group)),
            None => bail!("\"hooks.{event}\" in {SETTINGS_FILE} is not a list"),
        }
    }
    Ok(settings)
}

/// Installs or removes our hooks in a worktree. `true` when the file changed.
///
/// **Refused on a file git tracks**: writing there would put a change in the
/// review of a worktree whose agent never asked for one. Otherwise, once
/// written, the file is made invisible to git if nothing ignores it already —
/// Claude Code ignores it itself when **it** creates it, and here it is not
/// always the one that did.
pub fn configure(worktree: &Path, install: bool) -> Result<bool> {
    use crate::git::{git_ok, repo};

    let path = worktree.join(SETTINGS_FILE);
    if git_ok(
        worktree,
        &["ls-files", "--error-unmatch", "--", SETTINGS_FILE],
    ) {
        bail!(
            "{SETTINGS_FILE} is tracked by git in {}",
            worktree.display()
        );
    }
    let before = match std::fs::read_to_string(&path) {
        Ok(text) if text.trim().is_empty() => None,
        Ok(text) => Some(
            serde_json::from_str::<Value>(&text)
                .with_context(|| format!("{} is not valid JSON", path.display()))?,
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let after = match (install, before.clone()) {
        (true, before) => Some(with_ours(before)?),
        (false, Some(before)) => Some(without_ours(before)?),
        (false, None) => None,
    };
    if after == before {
        return Ok(false);
    }
    match after {
        // Nothing left but what we had put there: the file goes with it.
        Some(after) if !install && after.as_object().is_some_and(Map::is_empty) => {
            std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        }
        Some(after) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            let mut text = serde_json::to_string_pretty(&after)?;
            text.push('\n');
            let temporary = path.with_extension("json.claudhub");
            std::fs::write(&temporary, text)
                .with_context(|| format!("writing {}", temporary.display()))?;
            std::fs::rename(&temporary, &path)
                .with_context(|| format!("writing {}", path.display()))?;
        }
        None => return Ok(false),
    }
    if install && !git_ok(worktree, &["check-ignore", "-q", "--", SETTINGS_FILE]) {
        if let Some(git_dir) = repo::git_dir(worktree) {
            let common = repo::common_dir_on_disk(&git_dir).unwrap_or(git_dir);
            exclude(&common, &format!("/{SETTINGS_FILE}"))?;
        }
    }
    Ok(true)
}

/// Adds a line to `info/exclude`, which every worktree of the repository reads
/// and nobody commits.
fn exclude(common: &Path, pattern: &str) -> Result<()> {
    let path = common.join("info").join("exclude");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing.lines().any(|line| line.trim() == pattern) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(pattern);
    text.push('\n');
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(json: &str) -> Option<Signal> {
        signal_of(&serde_json::from_str(json).expect("json"))
    }

    #[test]
    fn each_hooked_event_names_a_state() {
        assert_eq!(
            event(r#"{"hook_event_name":"UserPromptSubmit","prompt":"x"}"#),
            Some(Signal::Working)
        );
        assert_eq!(
            event(r#"{"hook_event_name":"PostToolUse"}"#),
            Some(Signal::Working)
        );
        assert_eq!(
            event(r#"{"hook_event_name":"Stop","stop_hook_active":false}"#),
            Some(Signal::Finished)
        );
        assert_eq!(
            event(r#"{"hook_event_name":"SessionStart","source":"startup"}"#),
            Some(Signal::Ready)
        );
        assert_eq!(
            event(r#"{"hook_event_name":"SessionEnd","reason":"other"}"#),
            Some(Signal::Ended)
        );
        assert_eq!(
            event(
                r#"{"hook_event_name":"Notification","notification_type":"permission_prompt",
                    "message":"Claude needs your permission to use Bash"}"#
            ),
            Some(Signal::Waiting(
                "Claude needs your permission to use Bash".into()
            ))
        );
        assert_eq!(
            event(r#"{"hook_event_name":"Notification","notification_type":"idle_prompt"}"#),
            Some(Signal::Idle)
        );
    }

    #[test]
    fn what_is_not_a_state_is_no_word_at_all() {
        // A compaction happens mid-turn: "a session starts" would be a lie.
        assert_eq!(
            event(r#"{"hook_event_name":"SessionStart","source":"compact"}"#),
            None
        );
        assert_eq!(
            event(r#"{"hook_event_name":"Notification","notification_type":"auth_success"}"#),
            None
        );
        assert_eq!(event(r#"{"hook_event_name":"PreToolUse"}"#), None);
        // A `null` where a string is expected is read as nothing, not as a
        // failure of the whole event.
        assert_eq!(
            event(
                r#"{"hook_event_name":"Notification","notification_type":"permission_prompt","message":null}"#
            ),
            Some(Signal::Waiting(String::new()))
        );
    }

    #[test]
    fn a_long_message_is_cut_to_a_line() {
        let long = "x".repeat(1000);
        let json = format!(
            r#"{{"hook_event_name":"Notification","notification_type":"permission_prompt","message":"{long}\nsecond"}}"#
        );
        let Some(Signal::Waiting(message)) = event(&json) else {
            panic!("waiting");
        };
        assert_eq!(message.chars().count(), MESSAGE_CAP + 1);
        assert!(message.ends_with('…'));
    }

    #[test]
    fn a_file_is_the_pid_line_then_the_json() {
        let text = "4242\n{\"session_id\":\"ab-1\",\"cwd\":\"/p/repo/src\",\"hook_event_name\":\"Stop\"}\n";
        assert_eq!(
            parse(text),
            Some((
                Some(4242),
                "ab-1".into(),
                PathBuf::from("/p/repo/src"),
                Signal::Finished
            ))
        );
        // An unreadable pid does not lose the session.
        let text = "\n{\"session_id\":\"ab-1\",\"cwd\":\"/p\",\"hook_event_name\":\"Stop\"}";
        assert_eq!(parse(text).map(|parsed| parsed.0), Some(None));
        // Cut short: nothing.
        assert_eq!(parse("4242\n{\"session_id\":\"ab"), None);
    }

    fn users() -> Value {
        json!({
            "permissions": { "allow": ["Bash(ls:*)"] },
            "hooks": {
                "Stop": [
                    { "hooks": [{ "type": "command", "command": "notify-send done" }] }
                ]
            }
        })
    }

    #[test]
    fn installing_keeps_what_the_user_had() {
        let installed = with_ours(Some(users())).expect("installed");
        assert_eq!(installed["permissions"], users()["permissions"]);
        let stop = installed["hooks"]["Stop"].as_array().expect("Stop");
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[0]["hooks"][0]["command"], "notify-send done");
        assert!(is_ours(&stop[1]["hooks"][0]));
        for (event, matcher) in EVENTS {
            let groups = installed["hooks"][event].as_array().expect("event");
            let ours = groups.last().expect("ours");
            assert_eq!(ours.get("matcher").and_then(Value::as_str), *matcher);
        }
    }

    #[test]
    fn installing_twice_is_installing_once() {
        let once = with_ours(Some(users())).expect("once");
        let twice = with_ours(Some(once.clone())).expect("twice");
        assert_eq!(once, twice);
        assert_eq!(
            with_ours(None).expect("fresh"),
            with_ours(Some(json!({}))).expect("{}")
        );
    }

    #[test]
    fn removing_gives_back_the_file_as_it_was() {
        let installed = with_ours(Some(users())).expect("installed");
        assert_eq!(without_ours(installed).expect("removed"), users());
        // Nothing of the user's: nothing left at all.
        let alone = with_ours(None).expect("alone");
        assert_eq!(without_ours(alone).expect("removed"), json!({}));
    }

    #[test]
    fn an_entry_of_ours_among_the_users_goes_alone() {
        let mixed = json!({ "hooks": { "Stop": [ { "hooks": [
            { "type": "command", "command": "notify-send done" },
            { "type": "command", "command": HOOK_COMMAND }
        ] } ] } });
        assert_eq!(without_ours(mixed).expect("removed"), users_stop_only());
    }

    fn users_stop_only() -> Value {
        json!({ "hooks": { "Stop": [ { "hooks": [
            { "type": "command", "command": "notify-send done" }
        ] } ] } })
    }

    #[test]
    fn a_file_of_another_shape_is_refused_not_overwritten() {
        assert!(with_ours(Some(json!([1, 2]))).is_err());
        assert!(with_ours(Some(json!({ "hooks": "no" }))).is_err());
        assert!(with_ours(Some(json!({ "hooks": { "Stop": {} } }))).is_err());
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "claudhub-agent-hooks-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    /// The command, run by a real `sh` on what Claude Code sends: the file
    /// lands under the session's name, the text untouched — quotes, `$(…)`
    /// and backquotes are data, never code.
    #[cfg(unix)]
    #[test]
    fn the_hook_writes_the_session_file_and_says_nothing() {
        use std::io::Write as _;
        let home = scratch("home");
        let json = r#"{"session_id":"ab-12_c","cwd":"/p/repo","hook_event_name":"Stop","last_assistant_message":"said \"hi\" $(touch pwned) `x`"}"#;
        let run = |input: &str| {
            let mut child = std::process::Command::new("sh")
                .arg("-c")
                .arg(HOOK_COMMAND)
                .current_dir(&home)
                .env("HOME", &home)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("sh");
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(input.as_bytes())
                .expect("write");
            child.wait_with_output().expect("wait")
        };
        let out = run(json);
        assert!(out.status.success());
        assert!(
            out.stdout.is_empty(),
            "stdout goes into the agent's context"
        );
        let written =
            std::fs::read_to_string(home.join(".claudhub/agents/ab-12_c.json")).expect("file");
        let (pid, session, _, signal) = parse(&written).expect("parsed");
        assert!(pid.is_some());
        assert_eq!(session, "ab-12_c");
        assert_eq!(signal, Signal::Finished);
        assert!(!home.join("pwned").exists());
        // A session id that would climb out of the folder writes nothing.
        let out = run(r#"{"session_id":"../../x","hook_event_name":"Stop"}"#);
        assert!(out.status.success());
        // No session id at all, nothing either — and still no failure: exit 2
        // would block the prompt.
        assert!(run(r#"{"x":"y"}"#).status.success());
        let files: Vec<_> = std::fs::read_dir(home.join(".claudhub/agents"))
            .expect("folder")
            .flatten()
            .map(|entry| entry.file_name())
            .collect();
        assert_eq!(files, vec![std::ffi::OsString::from("ab-12_c.json")]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn reading_finds_the_worktree_and_prunes_what_is_over() {
        let folder = scratch("read");
        let write = |name: &str, text: &str| std::fs::write(folder.join(name), text).expect("w");
        write(
            "a.json",
            "7\n{\"session_id\":\"a\",\"cwd\":\"/p/repo/src\",\"hook_event_name\":\"Stop\"}",
        );
        write(
            "b.json",
            "7\n{\"session_id\":\"b\",\"cwd\":\"/elsewhere\",\"hook_event_name\":\"Stop\"}",
        );
        write(
            "c.json",
            "7\n{\"session_id\":\"c\",\"cwd\":\"/p/repo\",\"hook_event_name\":\"SessionEnd\"}",
        );
        write("c.123", "half a file");
        let worktrees = vec![PathBuf::from("/p/repo")];
        // Now: the closed session is still read, so the sweep sees it close.
        let now = SystemTime::now();
        let mut read = read_in(&folder, &worktrees, now);
        read.sort_by(|a, b| a.session.cmp(&b.session));
        let sessions: Vec<_> = read.iter().map(|r| r.session.as_str()).collect();
        assert_eq!(sessions, vec!["a", "c"]);
        assert_eq!(read[0].worktree, PathBuf::from("/p/repo"));
        assert_eq!(read[0].pid, Some(7));
        // Two minutes on: the closed one is gone from the disk, the other
        // repository's file is left where it is.
        let later = now + Duration::from_secs(120);
        let read = read_in(&folder, &worktrees, later);
        assert_eq!(read.len(), 1);
        assert!(!folder.join("c.json").exists());
        assert!(folder.join("b.json").exists());
        // Four days on: everything is forgotten.
        let _ = read_in(
            &folder,
            &worktrees,
            now + Duration::from_secs(4 * 24 * 3600),
        );
        assert!(!folder.join("a.json").exists());
        assert!(!folder.join("b.json").exists());
        let _ = std::fs::remove_dir_all(&folder);
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    }

    #[test]
    fn configuring_a_worktree_merges_and_keeps_git_quiet() {
        let repo = scratch("configure");
        git(&repo, &["init", "-q"]);
        std::fs::create_dir_all(repo.join(".claude")).expect(".claude");
        std::fs::write(
            repo.join(SETTINGS_FILE),
            serde_json::to_string(&users()).expect("json"),
        )
        .expect("write");
        // The user's own global ignore may already cover the file — Claude
        // Code adds it there — in which case `info/exclude` has nothing to do.
        let ignored_before =
            crate::git::git_ok(&repo, &["check-ignore", "-q", "--", SETTINGS_FILE]);
        assert!(configure(&repo, true).expect("install"));
        assert!(!configure(&repo, true).expect("again"), "idempotent");
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(repo.join(SETTINGS_FILE)).expect("r"))
                .expect("json");
        assert_eq!(written["permissions"], users()["permissions"]);
        // Nothing ignored it: `info/exclude` does now, and the status is clean.
        if !ignored_before {
            let exclude = std::fs::read_to_string(repo.join(".git/info/exclude")).expect("exclude");
            assert!(exclude
                .lines()
                .any(|line| line == "/.claude/settings.local.json"));
        }
        assert!(crate::git::git_ok(
            &repo,
            &["check-ignore", "-q", "--", SETTINGS_FILE]
        ));
        // Removed: the user's file, exactly.
        assert!(configure(&repo, false).expect("remove"));
        let back: Value =
            serde_json::from_str(&std::fs::read_to_string(repo.join(SETTINGS_FILE)).expect("r"))
                .expect("json");
        assert_eq!(back, users());
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn a_tracked_settings_file_is_not_touched() {
        let repo = scratch("tracked");
        git(&repo, &["init", "-q"]);
        std::fs::create_dir_all(repo.join(".claude")).expect(".claude");
        std::fs::write(repo.join(SETTINGS_FILE), "{}\n").expect("write");
        git(&repo, &["add", SETTINGS_FILE]);
        assert!(configure(&repo, true).is_err());
        assert_eq!(
            std::fs::read_to_string(repo.join(SETTINGS_FILE)).expect("r"),
            "{}\n"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }
}
