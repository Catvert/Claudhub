//! A project's Sentry issues, and what is needed to bring them near the code.
//!
//! Claudhub **reads** Sentry; it never sends it anything. An error report is a
//! starting point like any other — often better than an intention, because it
//! already carries the trace and the offending file — and the useful gesture is
//! to hand it to an agent along with the code around the application's frames.
//!
//! What the views show is modelled on Sentry's own page, and not out of
//! deference: an error is not read from a trace alone. What makes it
//! diagnosable is what surrounds it — how long it has been happening, how
//! often, on which release, in which environment, what the user was doing just
//! before. A trace without that context is read twice: here, then in the
//! browser.
//!
//! **This module is pure and it is where the decisions are**, in front of the
//! view that paints them: the URLs, the shapes the API returns, the filter, and
//! the prompt. Like every format we parse it is tested on a fixture — a remote
//! API changes without warning, and a renamed field shows up in a test here
//! rather than as an empty list at run time.
//!
//! It has been through the plugin system and come back. Nine hundred lines of
//! Rune proved the scripting API held; what the round trip left behind is this,
//! which was always the half that had nothing to do with scripting.

use std::path::Path;

use anyhow::{Context as _, Result};
use serde::Deserialize;

/// Sentry's public API. A self-hosted instance says so in the settings, which
/// is the only thing that changes.
pub const DEFAULT_HOST: &str = "https://sentry.io";

/// The query a fresh install reads with: what is not resolved.
pub const DEFAULT_QUERY: &str = "is:unresolved";

/// How many issues one page asks for.
const PER_PAGE: usize = 50;

/// How far back the list looks. Two weeks is what makes "how long has this been
/// happening" answerable without paging.
const PERIOD: &str = "14d";

/// How many breadcrumbs are kept. The **last** ones: they describe the second
/// before, and a list of a hundred drowns what one is looking for.
const CRUMBS: usize = 12;

/// An issue, cut down to what the views show.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Issue {
    pub id: String,
    /// `PROJECT-4F2`, the reference a team passes around and the tools take.
    pub short_id: String,
    /// The whole of what Sentry calls the title, kind and value together.
    pub title: String,
    /// `ValueError`, `TypeError`… the class that was raised.
    pub kind: String,
    /// What it said, when that adds something to the kind.
    pub value: String,
    /// Where it was raised, as Sentry writes it.
    pub culprit: String,
    /// `error`, `warning`, `fatal`…
    pub level: String,
    /// `unresolved`, `ignored`, `resolved`.
    pub status: String,
    pub count: u64,
    pub users: u64,
    /// As Sentry writes them (ISO 8601).
    pub first_seen: String,
    pub last_seen: String,
    pub permalink: String,
}

impl Issue {
    /// Whether the filter's word is in what the row shows.
    ///
    /// The title and the culprit, and nothing else: a filter that matched what
    /// is not on screen is a filter whose answers cannot be checked.
    pub fn matches(&self, needle: &str) -> bool {
        let needle = needle.trim().to_lowercase();
        needle.is_empty()
            || self.title.to_lowercase().contains(&needle)
            || self.culprit.to_lowercase().contains(&needle)
    }
}

/// One line of a call stack.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frame {
    /// Path as Sentry knows it. It is not always relative to the repository —
    /// hence `Frame::repo_path`, which does its best.
    pub filename: String,
    pub function: String,
    pub line: usize,
    /// Does the frame belong to the application's code, as against a
    /// dependency. That is the one we want to read.
    pub in_app: bool,
    /// The surrounding code, as Sentry returns it: `(number, line)`.
    ///
    /// It comes from the event, so from the code **deployed** at the time of
    /// the error: that is precisely what we want to quote, and re-reading it
    /// from disk would give today's version.
    pub context: Vec<(usize, String)>,
}

impl Frame {
    /// The path brought back to the repository, when possible.
    ///
    /// Sentry often writes an absolute server path
    /// (`/var/www/app/Http/Kernel.php`) or a module (`app.http.kernel`). We cut
    /// at the first segment that exists in the worktree; failing that we return
    /// the path as it stands and the user sees what Sentry said.
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

/// One step of the trail that led to the error.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Crumb {
    pub message: String,
    pub category: String,
    pub level: String,
}

/// One of an event's tags: the conditions it happened in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tag {
    pub key: String,
    pub value: String,
}

/// What one tag is worth across **every** occurrence.
///
/// The only reading of these views that is not about one event, and the one
/// that says whether a bug is everywhere or at a single customer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Spread {
    pub name: String,
    /// Value and share, in percent, most seen first.
    pub values: Vec<(String, u8)>,
}

/// An issue's most recent event: its stack, its context, its trail.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Event {
    pub message: String,
    pub tags: Vec<Tag>,
    /// The frames, oldest to newest — Sentry's order, which is a trace read top
    /// to bottom. The view reverses it, as Sentry's page does.
    pub frames: Vec<Frame>,
    pub crumbs: Vec<Crumb>,
}

// — The URLs ————————————————————————————————————————————————————————

/// The project's issues, most recent first.
pub fn issues_url(host: &str, org: &str, project: &str, query: &str) -> String {
    format!(
        "{}/api/0/projects/{org}/{project}/issues/?query={}&statsPeriod={PERIOD}&limit={PER_PAGE}",
        host.trim_end_matches('/'),
        escape(query),
    )
}

/// An issue's most recent event.
///
/// `/organizations/{org}/issues/{id}/events/` and not `…/events/latest/`, which
/// has gone from the API and answers 404. `full=true` is what brings the stack
/// back: without it the answer holds the metadata alone.
pub fn event_url(host: &str, org: &str, issue: &str) -> String {
    format!(
        "{}/api/0/organizations/{org}/issues/{issue}/events/?full=true&per_page=1",
        host.trim_end_matches('/'),
    )
}

/// What its tags are worth across every occurrence.
pub fn tags_url(host: &str, org: &str, issue: &str) -> String {
    format!(
        "{}/api/0/organizations/{org}/issues/{issue}/tags/",
        host.trim_end_matches('/'),
    )
}

/// Percent-encodes what a query may hold.
///
/// Written here rather than pulled in: what a Sentry query contains is
/// `is:unresolved environment:prod`, so spaces and colons and nothing exotic —
/// and a dependency for eight characters is a dependency to keep up to date.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

// — What the API returns ————————————————————————————————————————————
//
// Separate structures, `#[serde(default)]` everywhere: the API adds and removes
// fields, and a missing one must not empty the whole list.

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawIssue {
    id: String,
    #[serde(rename = "shortId")]
    short_id: String,
    title: String,
    culprit: String,
    level: String,
    status: String,
    /// Sentry writes the count as a **string** in the issue list and as a
    /// number elsewhere: the raw value is kept and converted by hand,
    /// otherwise half the responses fail to read.
    count: serde_json::Value,
    #[serde(rename = "userCount")]
    user_count: serde_json::Value,
    #[serde(rename = "firstSeen")]
    first_seen: String,
    #[serde(rename = "lastSeen")]
    last_seen: String,
    permalink: String,
    metadata: RawMeta,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawMeta {
    #[serde(rename = "type")]
    kind: String,
    value: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawEvent {
    message: String,
    entries: Vec<RawEntry>,
    tags: Vec<RawTag>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawTag {
    key: String,
    value: String,
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

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawSpread {
    key: String,
    name: String,
    #[serde(rename = "totalValues")]
    total: u64,
    #[serde(rename = "topValues")]
    top: Vec<RawSpreadValue>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawSpreadValue {
    value: String,
    count: u64,
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
            users: as_u64(&issue.user_count),
            id: issue.id,
            short_id: issue.short_id,
            // The kind alone when the metadata has one: the title repeats it
            // with the message glued on, and the two are two things — the class
            // that was raised, and what it said.
            kind: if issue.metadata.kind.is_empty() {
                issue.title.clone()
            } else {
                issue.metadata.kind
            },
            value: issue.metadata.value,
            title: issue.title,
            culprit: issue.culprit,
            level: issue.level,
            status: issue.status,
            first_seen: issue.first_seen,
            last_seen: issue.last_seen,
            permalink: issue.permalink,
        })
        .collect())
}

/// Reads the one event an issue's events endpoint returns.
///
/// The list is asked for with `per_page=1`, so it holds one or none: an issue
/// whose events have expired is not an error, it is an issue with nothing left
/// to read.
pub fn parse_event(json: &str) -> Result<Option<Event>> {
    let raw: Vec<RawEvent> =
        serde_json::from_str(json).context("unreadable Sentry response (event)")?;
    let Some(raw) = raw.into_iter().next() else {
        return Ok(None);
    };
    let mut frames = Vec::new();
    let mut crumbs = Vec::new();
    for entry in &raw.entries {
        // Both shapes exist depending on the SDK that sent the event, and
        // handling only one gives an empty trace on half the projects.
        match entry.kind.as_str() {
            "exception" => {
                let values = entry.data.get("values").and_then(|v| v.as_array());
                for value in values.into_iter().flatten() {
                    collect_frames(value.get("stacktrace"), &mut frames);
                }
            }
            "stacktrace" => collect_frames(Some(&entry.data), &mut frames),
            "breadcrumbs" => collect_crumbs(&entry.data, &mut crumbs),
            _ => {}
        }
    }
    Ok(Some(Event {
        message: raw.message,
        tags: raw
            .tags
            .into_iter()
            .filter(|tag| !tag.key.is_empty())
            .map(|tag| Tag {
                key: tag.key,
                value: tag.value,
            })
            .collect(),
        frames,
        crumbs,
    }))
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

/// The **last** breadcrumbs: they are the ones describing the second before.
fn collect_crumbs(data: &serde_json::Value, out: &mut Vec<Crumb>) {
    let Some(list) = data.get("values").and_then(|v| v.as_array()) else {
        return;
    };
    let start = list.len().saturating_sub(CRUMBS);
    for value in &list[start..] {
        let text = |key: &str| {
            value
                .get(key)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()
        };
        let crumb = Crumb {
            message: {
                let message = text("message");
                if message.is_empty() {
                    text("type")
                } else {
                    message
                }
            },
            category: text("category"),
            level: text("level"),
        };
        if crumb.message.is_empty() && crumb.category.is_empty() {
            continue;
        }
        out.push(crumb);
    }
}

/// Reads what each tag is worth across the issue's occurrences.
///
/// A tag with a single value is dropped: "environment: production, 100 %" is a
/// bar that says nothing, and seven of them push the trace off the screen.
pub fn parse_tags(json: &str) -> Result<Vec<Spread>> {
    let raw: Vec<RawSpread> =
        serde_json::from_str(json).context("unreadable Sentry response (tags)")?;
    Ok(raw
        .into_iter()
        .filter(|spread| spread.top.len() > 1)
        .map(|spread| Spread {
            name: if spread.name.is_empty() {
                spread.key
            } else {
                spread.name
            },
            values: spread
                .top
                .iter()
                .map(|value| {
                    let share = match spread.total {
                        0 => 0,
                        total => ((value.count * 100) / total).min(100) as u8,
                    };
                    (value.value.clone(), share)
                })
                .collect(),
        })
        .collect())
}

// — What goes to the agent ——————————————————————————————————————————

/// The prompt handed to an agent: the reference, the context, the trace, and
/// the code around the application's frames.
///
/// Frames outside the application are **quoted without their code**: a
/// framework stack is a hundred lines, and that is not where the bug is. A
/// free, tested function, like the notes': it is the piece to lock down.
///
/// The introduction arrives already translated from the view: `tr!` belongs to
/// the `ui` feature, and this module has to compile in the headless server.
pub fn prompt(
    intro: &str,
    org: &str,
    issue: &Issue,
    event: Option<&Event>,
    worktree: &Path,
) -> String {
    let mut out = String::new();
    out.push_str(intro);
    out.push_str("\n\n");

    // **What is copied is a photograph, and the agent may have the source.**
    // The text carries one instant's event; the questions that come next — is
    // it still happening, since which release, on how many users — are asked of
    // Sentry. An agent with the MCP server asks them itself, and all it was
    // missing was the identifier.
    let mut reference = Vec::new();
    if !issue.short_id.is_empty() {
        reference.push(format!("- Sentry issue: {}", issue.short_id));
    }
    if !issue.id.is_empty() {
        reference.push(format!("- Id: {}", issue.id));
    }
    if !org.is_empty() {
        reference.push(format!("- Organisation: {org}"));
    }
    if !issue.permalink.is_empty() {
        reference.push(format!("- {}", issue.permalink));
    }
    if !reference.is_empty() {
        out.push_str(&reference.join("\n"));
        out.push_str(
            "\n\nIf you have Sentry's MCP server, you can ask it for the rest with this \
             reference.\n\n",
        );
    }

    out.push_str(&format!("# {}\n", issue.kind));
    if !issue.value.is_empty() {
        out.push_str(&format!("{}\n", issue.value));
    }
    if !issue.culprit.is_empty() {
        out.push_str(&format!("{}\n", issue.culprit));
    }
    out.push_str(&format!(
        "{} occurrences, {} → {}\n",
        issue.count, issue.first_seen, issue.last_seen
    ));

    // The button is painted above the trace, so it can be pressed before the
    // trace arrives: what goes then is what is known, rather than an error.
    let Some(event) = event else {
        while out.ends_with('\n') {
            out.pop();
        }
        return out;
    };

    if !event.tags.is_empty() {
        out.push_str("\n## Context\n");
        for tag in &event.tags {
            out.push_str(&format!("- {}: {}\n", tag.key, tag.value));
        }
    }

    if !event.frames.is_empty() {
        out.push_str("\n## Trace\n");
        for frame in &event.frames {
            let path = frame.repo_path(worktree);
            out.push_str(&format!("- {path}:{}", frame.line));
            if !frame.function.is_empty() {
                out.push_str(&format!(" · {}", frame.function));
            }
            out.push('\n');
        }
    }

    for frame in event.frames.iter().filter(|frame| frame.in_app) {
        if frame.context.is_empty() {
            continue;
        }
        let path = frame.repo_path(worktree);
        out.push_str(&format!("\n## {path}:{}\n", frame.line));
        out.push_str("```\n");
        for (number, text) in &frame.context {
            // The offending line is marked, and it is not decoration: the text
            // reaches the agent without the gutter the panel paints, and "line
            // 46" in a block of eleven is counted by hand without it.
            let marker = if *number == frame.line { ">" } else { " " };
            out.push_str(&format!("{marker} {number:>5} {text}\n"));
        }
        out.push_str("```\n");
    }

    if !event.crumbs.is_empty() {
        out.push_str("\n## Breadcrumbs\n");
        for crumb in &event.crumbs {
            out.push_str(&format!("- {} · {}\n", crumb.category, crumb.message));
        }
    }

    while out.ends_with('\n') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One page of issues as the API writes it, cut to what we read: the count
    /// as a string, the metadata split from the title.
    const ISSUES: &str = r#"[
      {
        "id": "4207", "shortId": "SHOP-2F",
        "title": "ValueError: invalid literal for int()",
        "culprit": "app/checkout/total.py in compute",
        "level": "error", "status": "unresolved",
        "count": "312", "userCount": 47,
        "firstSeen": "2026-08-01T09:12:00Z", "lastSeen": "2026-08-29T07:44:00Z",
        "permalink": "https://sentry.io/organizations/acme/issues/4207/",
        "metadata": { "type": "ValueError", "value": "invalid literal for int()" }
      },
      { "id": "4208", "title": "TimeoutError", "count": 4 }
    ]"#;

    #[test]
    fn an_issue_is_read_with_its_reference_and_its_counts() {
        let issues = parse_issues(ISSUES).unwrap();
        assert_eq!(issues.len(), 2);
        let first = &issues[0];
        assert_eq!(first.short_id, "SHOP-2F");
        assert_eq!(first.kind, "ValueError");
        assert_eq!(first.value, "invalid literal for int()");
        // A string here and a number two fields down, both read.
        assert_eq!(first.count, 312);
        assert_eq!(first.users, 47);
        // Everything absent is absent, and nothing fails: the API adds and
        // removes fields, and one issue short of a field must not empty the
        // list.
        assert_eq!(issues[1].kind, "TimeoutError");
        assert_eq!(issues[1].count, 4);
        assert!(issues[1].short_id.is_empty());
    }

    #[test]
    fn the_filter_reads_the_title_and_the_culprit_and_nothing_else() {
        let issues = parse_issues(ISSUES).unwrap();
        assert!(issues[0].matches("valueerror"));
        assert!(issues[0].matches("checkout"));
        // The id is not on screen, so it is not what a filter answers on.
        assert!(!issues[0].matches("4207"));
        // An empty filter keeps everything.
        assert!(issues[1].matches("  "));
    }

    /// The two shapes an SDK sends a stack in, and the breadcrumbs beside them.
    const EVENT: &str = r#"[{
      "message": "boom",
      "tags": [{ "key": "environment", "value": "production" }, { "key": "", "value": "x" }],
      "entries": [
        { "type": "exception", "data": { "values": [ { "stacktrace": { "frames": [
            { "filename": "vendor/framework/run.py", "function": "handle", "lineNo": 12, "inApp": false },
            { "absPath": "/srv/app/checkout/total.py", "function": "compute", "lineNo": 46,
              "inApp": true, "context": [[45, "def compute(x):"], [46, "  return int(x)"]] }
        ] } } ] } },
        { "type": "breadcrumbs", "data": { "values": [
            { "message": "GET /cart", "category": "http", "level": "info" },
            { "type": "navigation", "category": "ui", "level": "info" }
        ] } }
      ]
    }]"#;

    #[test]
    fn an_event_carries_its_frames_its_tags_and_its_trail() {
        let event = parse_event(EVENT).unwrap().expect("one event");
        assert_eq!(event.frames.len(), 2);
        // Sentry's order is kept — oldest first; the view is what reverses it.
        assert_eq!(event.frames[0].filename, "vendor/framework/run.py");
        assert!(!event.frames[0].in_app);
        // `absPath` stands in for a missing `filename`.
        assert_eq!(event.frames[1].filename, "/srv/app/checkout/total.py");
        assert_eq!(event.frames[1].context.len(), 2);
        // A tag with no key is no tag.
        assert_eq!(event.tags.len(), 1);
        // A breadcrumb with no message falls back to its type.
        assert_eq!(event.crumbs.len(), 2);
        assert_eq!(event.crumbs[1].message, "navigation");
    }

    /// An issue whose events have expired is not an error.
    #[test]
    fn no_event_left_is_not_a_failure() {
        assert_eq!(parse_event("[]").unwrap(), None);
        assert!(parse_event("not json").is_err());
    }

    #[test]
    fn a_tag_worth_one_value_is_not_a_distribution() {
        let json = r#"[
          { "key": "release", "name": "Release", "totalValues": 200,
            "topValues": [ { "value": "1.4.0", "count": 150 }, { "value": "1.3.9", "count": 50 } ] },
          { "key": "environment", "totalValues": 200,
            "topValues": [ { "value": "production", "count": 200 } ] }
        ]"#;
        let spreads = parse_tags(json).unwrap();
        assert_eq!(spreads.len(), 1);
        assert_eq!(spreads[0].name, "Release");
        assert_eq!(
            spreads[0].values,
            vec![("1.4.0".into(), 75), ("1.3.9".into(), 25)]
        );
    }

    #[test]
    fn a_query_is_escaped_into_the_url() {
        let url = issues_url("https://sentry.io/", "acme", "shop", "is:unresolved x");
        assert!(url.starts_with("https://sentry.io/api/0/projects/acme/shop/issues/?query="));
        assert!(url.contains("is%3Aunresolved%20x"), "{url}");
        // The trailing slash of the host is not doubled.
        assert!(!url.contains("io//api"));
    }

    /// The prompt quotes every frame, and the code of the application's only.
    #[test]
    fn the_prompt_quotes_the_trace_and_the_code_that_is_ours() {
        let issues = parse_issues(ISSUES).unwrap();
        let event = parse_event(EVENT).unwrap().unwrap();
        let text = prompt(
            "Fix this.",
            "acme",
            &issues[0],
            Some(&event),
            Path::new("/nowhere"),
        );
        assert!(text.starts_with("Fix this."));
        assert!(text.contains("- Sentry issue: SHOP-2F"));
        assert!(text.contains("# ValueError"));
        // Both frames are listed…
        assert!(text.contains("vendor/framework/run.py:12"));
        assert!(text.contains("total.py:46"));
        // …and only ours is quoted, with the offending line marked.
        assert!(text.contains(">    46   return int(x)"), "{text}");
        assert!(!text.contains("## vendor/framework/run.py"));
        assert!(text.contains("## Breadcrumbs"));
        assert!(!text.ends_with('\n'));
    }

    /// The button sits above the trace, so it can be pressed before it lands.
    #[test]
    fn a_prompt_with_no_event_yet_says_what_is_known() {
        let issues = parse_issues(ISSUES).unwrap();
        let text = prompt("Fix this.", "acme", &issues[0], None, Path::new("/nowhere"));
        assert!(text.contains("# ValueError"));
        assert!(!text.contains("## Trace"));
    }
}
