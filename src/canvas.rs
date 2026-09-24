//! The home screen's nodes as files: what a note, a review or a diagram is on
//! disk, so that an agent can read and write them and a team can share them.
//!
//! **Files, and not our state**, because the point is that someone other than
//! this window writes them: Claude, asked for a review, writes one; a
//! colleague's Claude writes one on a branch that comes back with a `pull`.
//! A file in `.claudhub/` at the root of the checkout is versioned with the
//! code it speaks of — it travels with its branch, shows in the pull request,
//! and needs nothing but a Markdown reader to be read. A **private** node is
//! the same file in the worktree's vault folder, out of the repository.
//!
//! **One file per node**, named by its date and its title: two people writing
//! at once write two files, and git has nothing to merge.
//!
//! The layout — where a node stands, its size, folded or not — is not in the
//! file. It is each reader's own arrangement, and it lives in the store.
//!
//! Pure: the workers read and write the text, this says what it holds.

use std::path::{Path, PathBuf};

/// The folder at the root of a checkout that holds its shared nodes.
pub const DIR: &str = ".claudhub";
/// The notes' folder, in `DIR` or in the vault.
pub const NOTES: &str = "notes";
/// The mark at the head of every node file — also what `files::is_ours`
/// recognises, so the vault's sweeping never mistakes one for a stray file.
const MARK: &str = "claudhub";

/// What a node file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Kind {
    Note,
    Review,
    /// A picture — an SVG most of the time — with the body as its caption.
    Diagram,
}

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Kind::Note => "note",
            Kind::Review => "review",
            Kind::Diagram => "diagram",
        }
    }

    fn read(word: &str) -> Option<Kind> {
        match word {
            "note" => Some(Kind::Note),
            "review" => Some(Kind::Review),
            "diagram" => Some(Kind::Diagram),
            _ => None,
        }
    }
}

/// What a node hangs from on the plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Anchor {
    /// The repository — its git node.
    Repo,
    /// The worktree whose checkout holds the file.
    Worktree,
}

/// A node file, read.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Node {
    pub kind: Kind,
    pub anchor: Anchor,
    /// Said by the file, or `None`: a note's first line is then its title.
    pub title: Option<String>,
    pub author: Option<String>,
    /// The agent that wrote it, when one did.
    pub agent: Option<String>,
    pub created: Option<String>,
    /// `open`, `resolved`… — a review's.
    pub status: Option<String>,
    /// A diagram's picture, relative to the node file's folder.
    pub image: Option<String>,
    pub body: String,
}

impl Node {
    /// A fresh note, as the hand writes one.
    pub fn note(anchor: Anchor, author: Option<String>, created: String) -> Self {
        Self {
            kind: Kind::Note,
            anchor,
            title: None,
            author,
            agent: None,
            created: Some(created),
            status: None,
            image: None,
            body: String::new(),
        }
    }

    /// What the node is called: the file's title, or the body's first line
    /// bare of Markdown's marks.
    pub fn heading(&self) -> Option<String> {
        self.title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .or_else(|| {
                self.body
                    .lines()
                    .map(|line| line.trim_start_matches(['#', '>', '-', '*', ' ']).trim())
                    .find(|line| !line.is_empty())
                    .map(str::to_string)
            })
    }
}

/// Reads a node file. `None` for what does not carry our mark — a README
/// someone put in the folder is not a node.
pub fn parse(text: &str) -> Option<Node> {
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    let end = rest.find("\n---")?;
    let (head, tail) = rest.split_at(end);
    // Past the closing fence and the line it ends.
    let body = tail["\n---".len()..]
        .trim_start_matches('\r')
        .strip_prefix('\n')
        .unwrap_or("");
    let mut node = Node {
        kind: Kind::Note,
        anchor: Anchor::Worktree,
        title: None,
        author: None,
        agent: None,
        created: None,
        status: None,
        image: None,
        body: body.to_string(),
    };
    let mut marked = false;
    for line in head.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = unquote(value.trim());
        let text = (!value.is_empty()).then(|| value.to_string());
        match key.trim() {
            MARK => {
                node.kind = Kind::read(value)?;
                marked = true;
            }
            "anchor" => {
                node.anchor = if value == "repo" {
                    Anchor::Repo
                } else {
                    Anchor::Worktree
                }
            }
            "title" => node.title = text,
            "author" => node.author = text,
            "agent" => node.agent = text,
            "created" => node.created = text,
            "status" => node.status = text,
            "image" => node.image = text,
            _ => {}
        }
    }
    marked.then_some(node)
}

/// `"a: b"` as YAML quotes it, and the bare value as is.
fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value)
}

/// A value on one frontmatter line: quoted when YAML would read it as
/// something else — a colon, a leading mark.
fn quote(value: &str) -> String {
    let value = value.replace(['\n', '\r'], " ");
    let plain = !value.contains(": ")
        && !value.starts_with([
            '"', '\'', '#', '-', '[', '{', '&', '*', '!', '|', '>', '%', '@',
        ])
        && !value.ends_with(':');
    if plain {
        value
    } else {
        format!("\"{}\"", value.replace('"', "'"))
    }
}

/// Writes a node file: the frontmatter, flat, then the body.
pub fn render(node: &Node) -> String {
    let mut out = format!("---\n{MARK}: {}\n", node.kind.word());
    out.push_str(match node.anchor {
        Anchor::Repo => "anchor: repo\n",
        Anchor::Worktree => "anchor: worktree\n",
    });
    for (key, value) in [
        ("title", &node.title),
        ("author", &node.author),
        ("agent", &node.agent),
        ("created", &node.created),
        ("status", &node.status),
        ("image", &node.image),
    ] {
        if let Some(value) = value.as_deref().filter(|v| !v.is_empty()) {
            out.push_str(&format!("{key}: {}\n", quote(value)));
        }
    }
    out.push_str("---\n");
    out.push_str(&node.body);
    out
}

/// A title as a file name's part: lower case, words joined by dashes, no
/// more than a few of them.
pub fn slug(title: &str) -> String {
    let words: Vec<String> = title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(6)
        .map(|w| w.to_lowercase())
        .collect();
    if words.is_empty() {
        "note".into()
    } else {
        words.join("-")
    }
}

/// A new node's file name: `2026-09-24-2130-title.md`, a suffix added when
/// the name is taken. The time to the minute keeps a folder in the order the
/// nodes were written, and two people's files apart.
pub fn file_name(stamp: &str, title: &str, taken: &[String]) -> String {
    let base = format!("{stamp}-{}", slug(title));
    let mut name = format!("{base}.md");
    let mut n = 2;
    while taken.contains(&name) {
        name = format!("{base}-{n}.md");
        n += 1;
    }
    name
}

/// A finding of a review: a place in the code, what is said of it, and
/// where it stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Relative to the repository's root, as the review wrote it.
    pub path: String,
    /// One-based, as a review writes them; `end` is `start` for one line.
    pub start: u32,
    pub end: u32,
    pub severity: Option<String>,
    pub status: Option<String>,
    /// The quoted lines, when the review quoted them.
    pub excerpt: Option<String>,
    pub text: String,
    /// The line of the body its `### ` heading is on: what `set_status`
    /// finds it again by.
    pub at: usize,
}

impl Finding {
    pub fn resolved(&self) -> bool {
        self.status.as_deref() == Some("resolved")
    }
}

fn fence(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("```") || line.starts_with("~~~")
}

/// `src/a.rs:42` or `src/a.rs:40-44`, and whatever follows a space.
fn place(heading: &str) -> Option<(String, u32, u32)> {
    let first = heading.split_whitespace().next()?;
    let (path, lines) = first.rsplit_once(':')?;
    let (start, end) = match lines.split_once('-') {
        Some((start, end)) => (start.parse().ok()?, end.parse().ok()?),
        None => {
            let line = lines.parse().ok()?;
            (line, line)
        }
    };
    (!path.is_empty()).then(|| (path.to_string(), start, end))
}

/// A review's body read: its summary — what comes before `## Findings` —
/// and its findings, one per `### path:line` heading under it. What does not
/// read as a place is left in the text rather than guessed at.
pub fn findings(body: &str) -> (String, Vec<Finding>) {
    let lines: Vec<&str> = body.lines().collect();
    let mut in_fence = false;
    let mut start = None;
    for (index, line) in lines.iter().enumerate() {
        if fence(line) {
            in_fence = !in_fence;
        }
        if !in_fence && line.trim().eq_ignore_ascii_case("## findings") {
            start = Some(index);
            break;
        }
    }
    let Some(start) = start else {
        return (body.to_string(), Vec::new());
    };
    let summary = lines[..start].join("\n").trim_end().to_string();
    let mut found: Vec<Finding> = Vec::new();
    let mut in_fence = false;
    // Where the finding being read stands: its metadata is done once a blank
    // line or a fence has been met.
    let mut meta = false;
    let mut excerpt: Option<Vec<&str>> = None;
    for (index, line) in lines.iter().enumerate().skip(start + 1) {
        if !in_fence && line.starts_with("## ") && !line.starts_with("### ") {
            break;
        }
        if !in_fence && line.starts_with("### ") {
            if let Some((path, first, last)) = place(&line[4..]) {
                found.push(Finding {
                    path,
                    start: first,
                    end: last,
                    severity: None,
                    status: None,
                    excerpt: None,
                    text: String::new(),
                    at: index,
                });
                meta = true;
                excerpt = None;
                continue;
            }
        }
        let Some(finding) = found.last_mut() else {
            continue;
        };
        if fence(line) {
            in_fence = !in_fence;
            meta = false;
            match (in_fence, finding.excerpt.is_none()) {
                // The first fenced block is the excerpt.
                (true, true) if excerpt.is_none() => excerpt = Some(Vec::new()),
                (false, _) if excerpt.is_some() => {
                    finding.excerpt = excerpt.take().map(|l| l.join("\n"));
                }
                _ => {
                    finding.text.push_str(line);
                    finding.text.push('\n');
                }
            }
            continue;
        }
        if let Some(lines) = excerpt.as_mut() {
            lines.push(line);
            continue;
        }
        if meta {
            if line.trim().is_empty() {
                meta = false;
                continue;
            }
            if let Some((key, value)) = line.split_once(':') {
                let value = Some(value.trim().to_string()).filter(|v| !v.is_empty());
                match key.trim() {
                    "severity" => {
                        finding.severity = value;
                        continue;
                    }
                    "status" => {
                        finding.status = value;
                        continue;
                    }
                    _ => {}
                }
            }
            meta = false;
        }
        finding.text.push_str(line);
        finding.text.push('\n');
    }
    for finding in &mut found {
        finding.text = finding.text.trim().to_string();
    }
    (summary, found)
}

/// The body with one finding's `status:` set — the line changed in place,
/// or added under the heading when there was none. Everything else is left
/// as written.
pub fn set_status(body: &str, at: usize, status: &str) -> String {
    let mut lines: Vec<String> = body.lines().map(str::to_string).collect();
    if at >= lines.len() {
        return body.to_string();
    }
    let mut index = at + 1;
    let mut replaced = false;
    while index < lines.len() {
        let line = &lines[index];
        if line.trim().is_empty() || fence(line) || line.starts_with('#') {
            break;
        }
        if line
            .split_once(':')
            .is_some_and(|(key, _)| key.trim() == "status")
        {
            lines[index] = format!("status: {status}");
            replaced = true;
            break;
        }
        index += 1;
    }
    if !replaced {
        lines.insert(at + 1, format!("status: {status}"));
    }
    let mut out = lines.join("\n");
    if body.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// What an agent is asked to do about findings: fix them, then say so in the
/// review file itself — the tick a teammate's pull will read.
pub fn finding_prompt(review_file: &str, findings: &[&Finding]) -> String {
    let mut out = format!(
        "Please address these findings from the code review in `{review_file}`. \
         When one is fixed, set its `status:` to `resolved` in that file; if you \
         disagree with one, say why instead.\n"
    );
    for finding in findings {
        let place = if finding.start == finding.end {
            format!("{}:{}", finding.path, finding.start)
        } else {
            format!("{}:{}-{}", finding.path, finding.start, finding.end)
        };
        out.push_str(&format!(
            "\n## {place}{}\n",
            finding
                .severity
                .as_deref()
                .map(|s| format!(" ({s})"))
                .unwrap_or_default()
        ));
        if let Some(excerpt) = &finding.excerpt {
            out.push_str(&format!("\n```\n{excerpt}\n```\n"));
        }
        if !finding.text.is_empty() {
            out.push_str(&format!("\n{}\n", finding.text));
        }
    }
    out
}

/// Where a checkout's shared notes live.
pub fn shared_dir(checkout: &Path) -> PathBuf {
    crate::wslpath::join(&crate::wslpath::join(checkout, DIR), NOTES)
}

/// Where a worktree's private notes live, in its vault folder.
pub fn private_dir(vault: &Path) -> PathBuf {
    crate::wslpath::join(vault, "canvas")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_node_file_reads_back_what_was_written() {
        let node = Node {
            kind: Kind::Review,
            anchor: Anchor::Worktree,
            title: Some("Review: the cache".into()),
            author: Some("Arno Dupont".into()),
            agent: Some("claude".into()),
            created: Some("2026-09-24T21:30:00Z".into()),
            status: Some("open".into()),
            image: None,
            body: "# Summary\n\nAll good.\n".into(),
        };
        let text = render(&node);
        assert!(text.starts_with("---\nclaudhub: review\nanchor: worktree\n"));
        // A colon in a title is quoted, and comes back without the quotes.
        assert!(text.contains("title: \"Review: the cache\"\n"));
        assert_eq!(parse(&text), Some(node));
    }

    #[test]
    fn a_diagram_names_its_picture() {
        let node =
            parse("---\nclaudhub: diagram\nanchor: repo\nimage: flow.svg\n---\nThe flow.").unwrap();
        assert_eq!(node.kind, Kind::Diagram);
        assert_eq!(node.image.as_deref(), Some("flow.svg"));
        assert_eq!(parse(&render(&node)), Some(node));
    }

    #[test]
    fn a_file_without_our_mark_is_not_a_node() {
        assert_eq!(parse("# README\n"), None);
        assert_eq!(parse("---\ntitle: x\n---\nbody"), None);
        assert_eq!(parse("---\nclaudhub: poster\n---\n"), None);
    }

    #[test]
    fn an_agent_written_file_is_read_leniently() {
        // CRLF, a key we do not know, the body right after the fence.
        let text = "---\r\nclaudhub: note\r\nanchor: repo\r\nmood: good\r\n---\r\nHello";
        let node = parse(text).unwrap();
        assert_eq!(node.anchor, Anchor::Repo);
        assert_eq!(node.body, "Hello");
    }

    #[test]
    fn a_note_is_called_by_its_title_or_its_first_line() {
        let mut node = Node::note(Anchor::Worktree, None, "x".into());
        assert_eq!(node.heading(), None);
        node.body = "\n## Cache — to check\nmore".into();
        assert_eq!(node.heading().as_deref(), Some("Cache — to check"));
        node.title = Some("Given".into());
        assert_eq!(node.heading().as_deref(), Some("Given"));
    }

    const REVIEW: &str = "A careful change.\n\n## Findings\n\n### src/cache.rs:42\nseverity: warning\nstatus: open\n\n```rust\nlet key = format!(\"{}\", id);\n### not a heading inside a fence\n```\n\nThe key is built twice.\n\n### src/api.rs:10-12 — routes\nseverity: suggestion\n\nSplit this.\n\n### no place here\nstray line\n";

    #[test]
    fn a_review_reads_as_its_summary_and_its_findings() {
        let (summary, found) = findings(REVIEW);
        assert_eq!(summary, "A careful change.");
        assert_eq!(found.len(), 2);
        let first = &found[0];
        assert_eq!(
            (first.path.as_str(), first.start, first.end),
            ("src/cache.rs", 42, 42)
        );
        assert_eq!(first.severity.as_deref(), Some("warning"));
        assert_eq!(first.status.as_deref(), Some("open"));
        assert_eq!(
            first.excerpt.as_deref(),
            Some("let key = format!(\"{}\", id);\n### not a heading inside a fence")
        );
        assert_eq!(first.text, "The key is built twice.");
        let second = &found[1];
        assert_eq!((second.start, second.end), (10, 12));
        assert_eq!(second.status, None);
        // A heading that is no place stays in the text of the one before.
        assert!(second.text.contains("### no place here"));
        assert!(findings("No findings at all.").1.is_empty());
    }

    #[test]
    fn the_prompt_names_the_file_to_tick_and_quotes_each_place() {
        let (_, found) = findings(REVIEW);
        let prompt = finding_prompt(".claudhub/notes/r.md", &found.iter().collect::<Vec<_>>());
        assert!(prompt.contains("`.claudhub/notes/r.md`"));
        assert!(prompt.contains("`resolved`"));
        assert!(prompt.contains("## src/cache.rs:42 (warning)\n"));
        assert!(prompt.contains("## src/api.rs:10-12 (suggestion)\n"));
        assert!(prompt.contains("The key is built twice."));
    }

    #[test]
    fn resolving_a_finding_changes_its_line_and_nothing_else() {
        let (_, found) = findings(REVIEW);
        let resolved = set_status(REVIEW, found[0].at, "resolved");
        assert_eq!(resolved.lines().count(), REVIEW.lines().count());
        assert!(findings(&resolved).1[0].resolved());
        // Without a status line, one is added under the heading.
        let second = set_status(&resolved, found[1].at, "resolved");
        let again = findings(&second).1;
        assert!(again.iter().all(Finding::resolved));
        assert_eq!(second.lines().count(), REVIEW.lines().count() + 1);
        assert_eq!(again[0].text, "The key is built twice.");
    }

    #[test]
    fn a_file_name_is_its_date_and_its_title_and_never_taken() {
        assert_eq!(slug("Review: the cache, again!"), "review-the-cache-again");
        assert_eq!(slug("  "), "note");
        let taken = vec!["2026-09-24-2130-note.md".to_string()];
        assert_eq!(
            file_name("2026-09-24-2130", "", &taken),
            "2026-09-24-2130-note-2.md"
        );
        assert_eq!(
            file_name("2026-09-24-2130", "Cache", &taken),
            "2026-09-24-2130-cache.md"
        );
    }
}
