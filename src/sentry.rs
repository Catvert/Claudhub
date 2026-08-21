//! A project's Sentry issues, and what is needed to bring them near the code.
//!
//! Claudhub **reads** Sentry; it never sends it anything. An error report is a
//! starting point like any other — often better than an intention, because it
//! already carries the trace and the offending file — and the useful gesture is
//! to hand it to an agent along with the code around the application's frames.
//!
//! The token is read **from `SENTRY_TOKEN` first**, and only put in the
//! settings file for want of that: the file is 0600, which does not make it a
//! vault.
//!
//! Like every format we parse, this one is tested on a fixture: a remote API
//! changes without warning, and a renamed field shows up here rather than at
//! run time as an empty list.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use serde::Deserialize;

/// Sentry's public API. A self-hosted instance is configured through
/// `SENTRY_URL`, because that is the only thing that changes.
const DEFAULT_HOST: &str = "https://sentry.io";

/// A remote API sometimes takes several seconds; past that, it will not answer.
/// The same reasoning as the git command timeout.
const TIMEOUT: Duration = Duration::from_secs(20);

/// An issue, cut down to what the panel shows.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Issue {
    pub id: String,
    /// `ValueError`, `TypeError`… what Sentry calls the type.
    pub title: String,
    /// The message, when it adds something to the title.
    pub culprit: String,
    /// `error`, `warning`, `fatal`…
    pub level: String,
    pub count: u64,
    /// Last occurrence, as Sentry writes it (ISO 8601).
    pub last_seen: String,
    pub permalink: String,
}

/// One line of a call stack.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Frame {
    /// Path as Sentry knows it. It is not always relative to the repository —
    /// hence `Frame::repo_path`, which does its best.
    pub filename: String,
    pub function: String,
    pub line: usize,
    /// Does the frame belong to the application's code, as opposed to the
    /// dependencies. That is the one we want to read.
    pub in_app: bool,
    /// The surrounding code, as Sentry returns it: `(number, line)`.
    ///
    /// It comes from the event, so from the code **deployed** at the time of the
    /// error: that is precisely what we want to quote, and re-reading it from
    /// disk would give today's version.
    pub context: Vec<(usize, String)>,
}

impl Frame {
    /// The path brought back to the repository, when possible.
    ///
    /// Sentry often writes an absolute server path
    /// (`/var/www/app/Http/Kernel.php`) or a module (`app.http.kernel`). We cut
    /// at the first segment that exists in the worktree; failing that, we return
    /// the path as it is and the user sees what Sentry said.
    pub fn repo_path(&self, worktree: &Path) -> String {
        let normalized = self.filename.replace('\\', "/");
        let parts: Vec<&str> = normalized.split('/').filter(|p| !p.is_empty()).collect();
        for start in 0..parts.len() {
            let candidate = parts[start..].join("/");
            if worktree.join(&candidate).exists() {
                return candidate;
            }
        }
        normalized
    }
}

/// An issue's most recent event: its stack and its message.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Event {
    pub message: String,
    /// The frames, oldest to newest — Sentry's order, and that of a trace read
    /// top to bottom.
    pub frames: Vec<Frame>,
}

// — What the API returns ————————————————————————————————————————————
//
// Separate structures, `#[serde(default)]` everywhere: the API adds and removes
// fields, and a missing field must not empty the whole list.

#[derive(Deserialize)]
#[serde(default)]
struct RawIssue {
    id: String,
    title: String,
    culprit: String,
    level: String,
    count: serde_json::Value,
    #[serde(rename = "lastSeen")]
    last_seen: String,
    permalink: String,
}

impl Default for RawIssue {
    fn default() -> Self {
        Self {
            id: String::new(),
            title: String::new(),
            culprit: String::new(),
            level: String::new(),
            // Sentry writes the count as a **string** in the issue list and as a
            // number elsewhere: the raw value is kept and converted by hand,
            // otherwise half the responses fail to read.
            count: serde_json::Value::Null,
            last_seen: String::new(),
            permalink: String::new(),
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawEvent {
    message: String,
    entries: Vec<RawEntry>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawEntry {
    #[serde(rename = "type")]
    kind: String,
    data: serde_json::Value,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawFrame {
    filename: String,
    #[serde(rename = "absPath")]
    abs_path: String,
    module: String,
    function: String,
    #[serde(rename = "lineNo")]
    line_no: Option<usize>,
    #[serde(rename = "inApp")]
    in_app: bool,
    context: Vec<serde_json::Value>,
}

fn as_u64(value: &serde_json::Value) -> u64 {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0)
}

/// Reads a project's issue list.
pub fn parse_issues(json: &str) -> Result<Vec<Issue>> {
    let raw: Vec<RawIssue> =
        serde_json::from_str(json).context("unreadable Sentry response (issues)")?;
    Ok(raw
        .into_iter()
        .map(|issue| Issue {
            count: as_u64(&issue.count),
            id: issue.id,
            title: issue.title,
            culprit: issue.culprit,
            level: issue.level,
            last_seen: issue.last_seen,
            permalink: issue.permalink,
        })
        .collect())
}

/// Reads an issue's most recent event.
///
/// The stack lives in the `exception` or `stacktrace` entry of `entries`; both
/// shapes exist depending on the SDK that sent the event, and handling only one
/// gives an empty trace on half the projects.
pub fn parse_event(json: &str) -> Result<Event> {
    let raw: RawEvent = serde_json::from_str(json).context("unreadable Sentry response (event)")?;
    let mut frames = Vec::new();
    for entry in &raw.entries {
        match entry.kind.as_str() {
            "exception" => {
                let values = entry.data.get("values").and_then(|v| v.as_array());
                for value in values.into_iter().flatten() {
                    collect_frames(value.get("stacktrace"), &mut frames);
                }
            }
            "stacktrace" => collect_frames(Some(&entry.data), &mut frames),
            _ => {}
        }
    }
    Ok(Event {
        message: raw.message,
        frames,
    })
}

fn collect_frames(stacktrace: Option<&serde_json::Value>, out: &mut Vec<Frame>) {
    let Some(list) = stacktrace
        .and_then(|s| s.get("frames"))
        .and_then(|f| f.as_array())
    else {
        return;
    };
    for value in list {
        let Ok(raw) = serde_json::from_value::<RawFrame>(value.clone()) else {
            continue;
        };
        let filename = [raw.filename, raw.abs_path, raw.module]
            .into_iter()
            .find(|candidate| !candidate.is_empty())
            .unwrap_or_default();
        if filename.is_empty() {
            continue;
        }
        out.push(Frame {
            filename,
            function: raw.function,
            line: raw.line_no.unwrap_or(0),
            in_app: raw.in_app,
            // `context` is a list of `[number, source]` pairs; anything not of
            // that shape is ignored rather than failing the read of the whole
            // trace.
            context: raw
                .context
                .iter()
                .filter_map(|pair| {
                    let pair = pair.as_array()?;
                    let line = as_u64(pair.first()?) as usize;
                    let text = pair.get(1)?.as_str().unwrap_or_default().to_string();
                    Some((line, text))
                })
                .collect(),
        });
    }
}

/// The prompt handed to an agent: the title, the message, the trace, and the
/// code around the application's frames.
///
/// Frames outside the application are **quoted without their code**: a framework
/// stack is a hundred lines, and that is not where the bug is. A free, tested
/// function, like the notes': it is the piece to lock down.
///
/// The introduction arrives already translated from the view: `tr!` belongs to
/// the `ui` feature, and this module has to compile in the headless server.
pub fn prompt(intro: &str, issue: &Issue, event: &Event, worktree: &Path) -> String {
    let mut out = String::new();
    out.push_str(intro);
    out.push_str("\n\n");
    if !issue.culprit.is_empty() {
        out.push_str(&issue.culprit);
        out.push('\n');
    }
    if !event.message.is_empty() && event.message != issue.title {
        out.push_str(&event.message);
        out.push('\n');
    }
    out.push('\n');

    for frame in &event.frames {
        let path = frame.repo_path(worktree);
        out.push_str(&format!("- {path}:{}", frame.line));
        if !frame.function.is_empty() {
            out.push_str(&format!(" · {}", frame.function));
        }
        out.push('\n');
    }

    for frame in event.frames.iter().filter(|frame| frame.in_app) {
        if frame.context.is_empty() {
            continue;
        }
        let path = frame.repo_path(worktree);
        out.push_str(&format!("\n## {path}:{}\n", frame.line));
        out.push_str("```\n");
        for (number, text) in &frame.context {
            // The offending line is marked: it is the only piece of information
            // the numbering does not give at a glance.
            let marker = if *number == frame.line { ">" } else { " " };
            out.push_str(&format!("{marker} {number:>5} {text}\n"));
        }
        out.push_str("```\n");
    }
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

/// The API token: the environment first, the settings file failing that.
pub fn token(fallback: &str) -> Option<String> {
    std::env::var("SENTRY_TOKEN")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| Some(fallback.trim().to_string()).filter(|value: &String| !value.is_empty()))
}

fn host() -> String {
    std::env::var("SENTRY_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim_end_matches('/').to_string())
        .unwrap_or_else(|| DEFAULT_HOST.to_string())
}

fn get(url: &str, token: &str) -> Result<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .build()
        .into();
    let mut response = agent
        .get(url)
        .header("Authorization", &format!("Bearer {token}"))
        .call()
        .with_context(|| format!("Sentry: {url} unreachable"))?;
    let status = response.status();
    if !status.is_success() {
        bail!("Sentry answered {status}");
    }
    response
        .body_mut()
        .read_to_string()
        .context("unreadable Sentry response")
}

/// A project's unresolved issues, most frequent first.
pub fn issues(org: &str, project: &str, query: &str, token: &str) -> Result<Vec<Issue>> {
    if org.trim().is_empty() || project.trim().is_empty() {
        bail!("Sentry organisation or project not configured");
    }
    let query = if query.trim().is_empty() {
        "is:unresolved".to_string()
    } else {
        query.trim().to_string()
    };
    let url = format!(
        "{}/api/0/projects/{}/{}/issues/?query={}&statsPeriod=14d",
        host(),
        urlencode(org),
        urlencode(project),
        urlencode(&query)
    );
    parse_issues(&get(&url, token)?)
}

/// An issue's most recent event.
pub fn latest_event(issue: &str, token: &str) -> Result<Event> {
    let url = format!(
        "{}/api/0/issues/{}/events/latest/",
        host(),
        urlencode(issue)
    );
    parse_event(&get(&url, token)?)
}

/// Minimal encoding of a URL component.
///
/// A Sentry query contains spaces and colons
/// (`is:unresolved environment:production`): letting them through as they are
/// gives an invalid URL, and pulling in a dependency for three characters would
/// be dear.
fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ISSUES: &str = r#"[
      {
        "id": "4508",
        "title": "TypeError: Cannot read properties of undefined",
        "culprit": "app/Http/Controllers/QuoteController.php in store",
        "level": "error",
        "count": "137",
        "lastSeen": "2026-08-19T10:12:00Z",
        "permalink": "https://sentry.io/organizations/acme/issues/4508/"
      },
      {
        "id": "4509",
        "title": "ValueError",
        "count": 3,
        "lastSeen": "2026-08-18T22:00:00Z"
      }
    ]"#;

    const EVENT: &str = r#"{
      "message": "Cannot read properties of undefined (reading 'total')",
      "entries": [
        {
          "type": "exception",
          "data": {
            "values": [
              {
                "stacktrace": {
                  "frames": [
                    {
                      "filename": "vendor/laravel/framework/src/Foundation/Http/Kernel.php",
                      "function": "handle",
                      "lineNo": 141,
                      "inApp": false
                    },
                    {
                      "filename": "app/Http/Controllers/QuoteController.php",
                      "function": "store",
                      "lineNo": 88,
                      "inApp": true,
                      "context": [
                        [86, "    public function store(Request $request)"],
                        [87, "    {"],
                        [88, "        return $request->quote->total;"],
                        [89, "    }"]
                      ]
                    }
                  ]
                }
              }
            ]
          }
        }
      ]
    }"#;

    #[test]
    fn issues_survive_a_count_written_as_a_string() {
        // Sentry writes the count as a string in the list and as a number
        // elsewhere: reading both is what avoids an empty list every other
        // day.
        let issues = parse_issues(ISSUES).unwrap();
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].id, "4508");
        assert_eq!(issues[0].count, 137);
        assert_eq!(issues[0].level, "error");
        // An issue with no culprit and no permalink still reads.
        assert_eq!(issues[1].count, 3);
        assert!(issues[1].culprit.is_empty());
    }

    #[test]
    fn a_stack_trace_keeps_its_order_and_its_context() {
        let event = parse_event(EVENT).unwrap();
        assert_eq!(event.frames.len(), 2);
        assert!(!event.frames[0].in_app);
        assert!(event.frames[1].in_app);
        assert_eq!(event.frames[1].line, 88);
        assert_eq!(event.frames[1].context.len(), 4);
        assert_eq!(event.frames[1].context[2].0, 88);
    }

    #[test]
    fn an_empty_response_is_not_an_error() {
        assert!(parse_issues("[]").unwrap().is_empty());
        assert!(parse_event("{}").unwrap().frames.is_empty());
    }

    #[test]
    fn the_prompt_lists_the_stack_and_quotes_only_the_application_code() {
        let issue = parse_issues(ISSUES).unwrap().remove(0);
        let event = parse_event(EVENT).unwrap();
        let text = prompt(
            "Here is a Sentry error.",
            &issue,
            &event,
            Path::new("/nowhere"),
        );
        // The whole stack, framework frames included: it is the path that led
        // there.
        assert!(text.contains("Kernel.php:141"), "{text}");
        assert!(text.contains("QuoteController.php:88"), "{text}");
        // But the code only for what belongs to the application: a framework
        // stack is a hundred lines, and the bug is not in it.
        assert!(
            text.contains("## app/Http/Controllers/QuoteController.php:88"),
            "{text}"
        );
        assert!(!text.contains("## vendor/"), "{text}");
        // The offending line is marked.
        assert!(text.contains(">    88 "), "{text}");
        assert!(!text.ends_with('\n'), "{text:?}");
    }

    #[test]
    fn a_frame_path_is_brought_back_to_the_repository() {
        let dir = std::env::temp_dir().join(format!("claudhub-sentry-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("app/Http")).unwrap();
        std::fs::write(dir.join("app/Http/Kernel.php"), "").unwrap();

        let frame = Frame {
            // The server path, as Sentry knows it.
            filename: "/var/www/releases/42/app/Http/Kernel.php".into(),
            ..Default::default()
        };
        assert_eq!(frame.repo_path(&dir), "app/Http/Kernel.php");

        // What cannot be found is returned as it is: better to show what Sentry
        // said than an invented path.
        let unknown = Frame {
            filename: "node_modules/x/index.js".into(),
            ..Default::default()
        };
        assert_eq!(unknown.repo_path(&dir), "node_modules/x/index.js");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_query_survives_its_spaces_and_colons() {
        assert_eq!(
            urlencode("is:unresolved environment:production"),
            "is%3Aunresolved%20environment%3Aproduction"
        );
    }
}
