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
}

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Kind::Note => "note",
            Kind::Review => "review",
        }
    }

    fn read(word: &str) -> Option<Kind> {
        match word {
            "note" => Some(Kind::Note),
            "review" => Some(Kind::Review),
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
            body: "# Summary\n\nAll good.\n".into(),
        };
        let text = render(&node);
        assert!(text.starts_with("---\nclaudhub: review\nanchor: worktree\n"));
        // A colon in a title is quoted, and comes back without the quotes.
        assert!(text.contains("title: \"Review: the cache\"\n"));
        assert_eq!(parse(&text), Some(node));
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
