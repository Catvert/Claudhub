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
    ///
    /// `exists` says whether a repository-relative path names a file of the
    /// worktree — see `locate` for why it is asked rather than looked up.
    pub fn repo_path(&self, exists: impl Fn(&str) -> bool) -> String {
        self.locate(exists)
            .unwrap_or_else(|| self.filename.replace('\\', "/"))
    }

    /// The path inside the worktree this frame names, or `None` when none of
    /// its tails is a file there — which is what says the frame opens nothing.
    ///
    /// **Never a path that climbs out.** The frame comes from an event, and
    /// an event is anyone's to send — a project's DSN is public, it ships in
    /// the page's JavaScript — so `../../../.bashrc` is a frame like any other,
    /// and the editor it opens writes. A tail holding a `..`, or anything the
    /// host reads as a root or a drive (`C:` under Windows), is refused before
    /// `exists` is asked; the tails after it are still tried, and they are
    /// inside the worktree by construction.
    ///
    /// `exists` and not the worktree's path, because the answer is not
    /// always on this machine's disk: under Windows the worktree is a Linux
    /// path of the distribution, which `Path::exists` looks for on drive `C:`.
    /// The view hands the explorer's file list when it has one, and the disk
    /// (`on_disk`) otherwise.
    pub fn locate(&self, exists: impl Fn(&str) -> bool) -> Option<String> {
        let normalized = self.filename.replace('\\', "/");
        let parts: Vec<&str> = normalized
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .collect();
        (0..parts.len())
            .map(|start| parts[start..].join("/"))
            .filter(|candidate| inside(candidate))
            .find(|candidate| exists(candidate))
    }
}

/// True when `candidate` is a relative path that stays below where it is
/// joined: plain names only, on every platform's reading of it.
fn inside(candidate: &str) -> bool {
    !candidate.split('/').any(|part| part == "..")
        && Path::new(candidate)
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

/// The disk's answer to `Frame::locate`: what the worktree holds, read where
/// this process runs — right on Linux, and blind under Windows, where the
/// worktree is the distribution's.
pub fn on_disk(worktree: &Path) -> impl Fn(&str) -> bool + '_ {
    move |candidate| worktree.join(candidate).exists()
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
// **Read field by field, never deserialised into a struct** — every answer,
// not only the frames (see `collect_frames` for what it cost there).
// `#[serde(default)]` covers a field that is *absent*; one that is present and
// `null` fails the whole struct, and a struct is one row of a list: a single
// issue with a `null` culprit or permalink emptied the whole page. Only the
// outer array is demanded — an answer that is not one is not Sentry's.

/// The answer's outer list. Anything but an array is a response we cannot
/// read, which is the one failure worth an error.
fn list_of(json: &str, what: &str) -> Result<Vec<serde_json::Value>> {
    serde_json::from_str::<Vec<serde_json::Value>>(json)
        .with_context(|| format!("unreadable Sentry response ({what})"))
}

/// A text field: absent, `null` and not-a-string all read as empty.
fn text_of(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string()
}

/// A list field: absent, `null` and not-a-list all read as empty.
fn items_of<'a>(value: &'a serde_json::Value, key: &str) -> &'a [serde_json::Value] {
    value
        .get(key)
        .and_then(|value| value.as_array())
        .map(Vec::as_slice)
        .unwrap_or_default()
}

/// A number, which Sentry writes as a **string** in the issue list and as a
/// number elsewhere: both are read, otherwise half the responses fail.
fn as_u64(value: &serde_json::Value) -> u64 {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0)
}

/// A number field, absent or `null` reading as zero.
fn count_of(value: &serde_json::Value, key: &str) -> u64 {
    value.get(key).map(as_u64).unwrap_or(0)
}

/// Reads a project's issue list.
pub fn parse_issues(json: &str) -> Result<Vec<Issue>> {
    Ok(list_of(json, "issues")?
        .iter()
        .filter(|issue| issue.is_object())
        .map(|issue| {
            let title = text_of(issue, "title");
            let metadata = issue.get("metadata").unwrap_or(&serde_json::Value::Null);
            let kind = text_of(metadata, "type");
            Issue {
                id: text_of(issue, "id"),
                short_id: text_of(issue, "shortId"),
                // The kind alone when the metadata has one: the title repeats
                // it with the message glued on, and the two are two things —
                // the class that was raised, and what it said.
                kind: if kind.is_empty() { title.clone() } else { kind },
                value: text_of(metadata, "value"),
                title,
                culprit: text_of(issue, "culprit"),
                level: text_of(issue, "level"),
                status: text_of(issue, "status"),
                count: count_of(issue, "count"),
                users: count_of(issue, "userCount"),
                first_seen: text_of(issue, "firstSeen"),
                last_seen: text_of(issue, "lastSeen"),
                permalink: text_of(issue, "permalink"),
            }
        })
        .collect())
}

/// Reads the one event an issue's events endpoint returns.
///
/// The list is asked for with `per_page=1`, so it holds one or none: an issue
/// whose events have expired is not an error, it is an issue with nothing left
/// to read.
pub fn parse_event(json: &str) -> Result<Option<Event>> {
    let events = list_of(json, "event")?;
    let Some(raw) = events.first() else {
        return Ok(None);
    };
    let mut frames = Vec::new();
    let mut crumbs = Vec::new();
    for entry in items_of(raw, "entries") {
        let data = entry.get("data").unwrap_or(&serde_json::Value::Null);
        // Both shapes exist depending on the SDK that sent the event, and
        // handling only one gives an empty trace on half the projects.
        match text_of(entry, "type").as_str() {
            "exception" => {
                for value in items_of(data, "values") {
                    collect_frames(value.get("stacktrace"), &mut frames);
                }
            }
            "stacktrace" => collect_frames(Some(data), &mut frames),
            "breadcrumbs" => collect_crumbs(data, &mut crumbs),
            _ => {}
        }
    }
    Ok(Some(Event {
        message: text_of(raw, "message"),
        tags: items_of(raw, "tags")
            .iter()
            .map(|tag| Tag {
                key: text_of(tag, "key"),
                value: text_of(tag, "value"),
            })
            .filter(|tag| !tag.key.is_empty())
            .collect(),
        frames,
        crumbs,
    }))
}

/// A frame is read **field by field**, never deserialised into a struct.
///
/// `#[serde(default)]` fills in a field that is **absent**; it does nothing for
/// one that is present and `null`, which fails the whole struct. Sentry writes
/// `null` freely — a PHP frame with no `module`, a vendor frame with no
/// `context` — so one such field dropped every frame of the trace on the floor
/// and the page showed the tags, the distribution and the breadcrumbs with no
/// stack between them. Read this way, a null is simply a field that is not
/// there, which is what it means.
fn collect_frames(stacktrace: Option<&serde_json::Value>, out: &mut Vec<Frame>) {
    let Some(list) = stacktrace
        .and_then(|s| s.get("frames"))
        .and_then(|f| f.as_array())
    else {
        return;
    };
    for value in list {
        let text = |key: &str| {
            value
                .get(key)
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string()
        };
        // The first of the three that says anything: Sentry names a frame by a
        // path, by an absolute path, or by a module, depending on the SDK.
        let filename = [text("filename"), text("absPath"), text("module")]
            .into_iter()
            .find(|candidate| !candidate.is_empty())
            .unwrap_or_default();
        if filename.is_empty() {
            continue;
        }
        // `context` is a list of `[number, source]` pairs; anything not of that
        // shape is ignored rather than failing the read of the whole trace.
        let context = value
            .get("context")
            .and_then(|context| context.as_array())
            .map(|pairs| {
                pairs
                    .iter()
                    .filter_map(|pair| {
                        let pair = pair.as_array()?;
                        let line = as_u64(pair.first()?) as usize;
                        let text = pair.get(1)?.as_str().unwrap_or_default().to_string();
                        Some((line, text))
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.push(Frame {
            filename,
            function: text("function"),
            line: value.get("lineNo").map(as_u64).unwrap_or(0) as usize,
            in_app: value
                .get("inApp")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
            context,
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
    Ok(list_of(json, "tags")?
        .iter()
        .filter(|spread| items_of(spread, "topValues").len() > 1)
        .map(|spread| {
            let total = count_of(spread, "totalValues");
            let name = text_of(spread, "name");
            Spread {
                name: if name.is_empty() {
                    text_of(spread, "key")
                } else {
                    name
                },
                values: items_of(spread, "topValues")
                    .iter()
                    .map(|value| {
                        let share =
                            match total {
                                0 => 0,
                                total => (count_of(value, "count").saturating_mul(100) / total)
                                    .min(100) as u8,
                            };
                        (text_of(value, "value"), share)
                    })
                    .collect(),
            }
        })
        .collect())
}

/// An ISO 8601 instant as Sentry writes it, read into a timestamp.
///
/// **Parsed here and not shown raw.** `2026-08-29T00:15:45.042637Z` is a fact
/// about a machine, and what one wants of "last seen" is how long ago — the
/// page said the header of an HTTP response where it meant a date.
///
/// By hand and not by a date parser: the shape is fixed, the field is Sentry's
/// own, and what is wanted from it is six numbers. Anything else gives `None`,
/// which the view shows as the text it was given — a format that changes must
/// degrade to "unreadable", never to a wrong date.
pub fn instant_of(text: &str) -> Option<i64> {
    let (date, rest) = text.split_once('T')?;
    let mut date = date.split('-');
    let (year, month, day) = (date.next()?, date.next()?, date.next()?);
    // UTC or nothing. The fraction is dropped — the second is the finest thing
    // this page ever says — but an **offset** is not: reading `+02:00` and
    // ignoring it would show a time two hours out with nothing to say so, which
    // is the one failure this function exists to refuse. Sentry writes `Z`.
    let rest = rest.strip_suffix('Z').unwrap_or(rest);
    if rest.contains('+') || rest.matches('-').count() > 0 {
        return None;
    }
    let time = rest.split('.').next()?;
    let mut time = time.split(':');
    let (hour, minute, second) = (time.next()?, time.next()?, time.next()?);
    let at = chrono::NaiveDate::from_ymd_opt(
        year.parse().ok()?,
        month.parse().ok()?,
        day.parse().ok()?,
    )?
    .and_hms_opt(
        hour.parse().ok()?,
        minute.parse().ok()?,
        second.parse().ok()?,
    )?;
    Some(at.and_utc().timestamp())
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
///
/// `exists` is what brings a frame's path back to the repository — see
/// `Frame::locate`.
pub fn prompt(
    intro: &str,
    org: &str,
    issue: &Issue,
    event: Option<&Event>,
    exists: &dyn Fn(&str) -> bool,
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
            let path = frame.repo_path(exists);
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
        let path = frame.repo_path(exists);
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
    fn an_instant_is_read_to_the_second_and_a_shape_we_do_not_know_is_not_guessed() {
        assert_eq!(instant_of("2026-08-29T00:15:45.042637Z"), Some(1787962545));
        // The fraction is optional.
        assert_eq!(instant_of("2026-08-29T00:15:45Z"), Some(1787962545));
        // **An offset is refused, not ignored.** Reading it and dropping it
        // would show a time two hours out with nothing to say so, which is the
        // one failure this exists to refuse.
        assert_eq!(instant_of("2026-08-29T00:15:45+02:00"), None);
        // A shape we do not know degrades to "unreadable", never to a wrong
        // date: the view then shows the text it was given.
        assert_eq!(instant_of("29/08/2026"), None);
        assert_eq!(instant_of(""), None);
        assert_eq!(instant_of("2026-13-45T99:99:99Z"), None);
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
        let text = prompt("Fix this.", "acme", &issues[0], Some(&event), &nowhere);
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
        let text = prompt("Fix this.", "acme", &issues[0], None, &nowhere);
        assert!(text.contains("# ValueError"));
        assert!(!text.contains("## Trace"));
    }

    /// A worktree that holds nothing: every frame keeps what Sentry said.
    fn nowhere(_: &str) -> bool {
        false
    }

    /// Sentry writes `null` where it has nothing, and one such field in one
    /// row must not cost the list, the event or the distribution.
    #[test]
    fn a_null_anywhere_is_a_field_that_is_not_there() {
        let issues = parse_issues(
            r#"[
              { "id": "1", "title": "Boom", "culprit": null, "permalink": null,
                "shortId": null, "count": null, "userCount": null,
                "metadata": null },
              { "id": "2", "title": "Bang", "metadata": { "type": null, "value": "x" } },
              null
            ]"#,
        )
        .unwrap();
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].id, "1");
        assert!(issues[0].culprit.is_empty() && issues[0].permalink.is_empty());
        assert_eq!(issues[0].kind, "Boom");
        assert_eq!(issues[0].count, 0);
        assert_eq!(issues[1].kind, "Bang");
        assert_eq!(issues[1].value, "x");

        let event = parse_event(
            r#"[{
              "message": null,
              "tags": [{ "key": "release", "value": null }, { "key": null, "value": "x" }],
              "entries": [
                { "type": "stacktrace", "data": { "frames": [
                    { "filename": "app/x.py", "lineNo": null, "inApp": null, "context": null }
                ] } },
                { "type": null, "data": null },
                { "type": "breadcrumbs", "data": null }
              ]
            }]"#,
        )
        .unwrap()
        .expect("one event");
        assert!(event.message.is_empty());
        assert_eq!(event.tags.len(), 1);
        assert_eq!(event.frames.len(), 1);
        assert_eq!(event.frames[0].line, 0);
        // `entries` itself may be null.
        assert!(parse_event(r#"[{ "entries": null, "tags": null }]"#)
            .unwrap()
            .is_some());

        let spreads = parse_tags(
            r#"[{ "key": "browser", "name": null, "totalValues": null,
                  "topValues": [ { "value": null, "count": 3 }, { "value": "Firefox", "count": null } ] }]"#,
        )
        .unwrap();
        assert_eq!(spreads.len(), 1);
        assert_eq!(spreads[0].name, "browser");
        // No total: no share, rather than a division by zero.
        assert_eq!(spreads[0].values[1], ("Firefox".into(), 0));
    }

    /// An event is anyone's to send, and a frame that climbs out of the
    /// worktree must open nothing — the editor it would open writes.
    #[test]
    fn a_frame_never_names_a_file_outside_the_worktree() {
        let frame = |filename: &str| Frame {
            filename: filename.into(),
            ..Default::default()
        };
        let everything = |_: &str| true;
        // A climb is refused; the tail below it is inside by construction.
        assert_eq!(
            frame("../../../.bashrc").locate(everything).as_deref(),
            Some(".bashrc")
        );
        let asked = std::cell::RefCell::new(Vec::new());
        let record = |candidate: &str| {
            asked.borrow_mut().push(candidate.to_string());
            false
        };
        assert_eq!(frame("app/../../etc/passwd").locate(record), None);
        assert!(asked.borrow().iter().all(|path| !path.contains("..")));
        assert!(asked.borrow().contains(&"etc/passwd".to_string()));
        // `.` says nothing, and a Windows path is cut like a Unix one.
        let known = |candidate: &str| candidate == "src/Http/Kernel.php";
        assert_eq!(
            frame(r"C:\inetpub\app\.\src\Http\Kernel.php")
                .locate(known)
                .as_deref(),
            Some("src/Http/Kernel.php")
        );
        // Nothing found: the frame opens nothing, and shows what Sentry said.
        let module = frame("app.http.kernel");
        assert_eq!(module.locate(nowhere), None);
        assert_eq!(module.repo_path(nowhere), "app.http.kernel");
    }
}
