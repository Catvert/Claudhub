//! Tags: what a repository has marked, and the four things one does with one.
//!
//! A tag is the only git object a review workflow creates *deliberately* — a
//! release, a milestone, a point one wants to come back to — and it was the one
//! thing Claudhub could neither see nor make. Reaching for a terminal to type
//! `git tag -a v1.2.0 -m …` beside a window that shows the history is exactly
//! the round trip this tool exists to remove.
//!
//! **Local and remote are two different pieces of knowledge**, and this module
//! keeps them apart. Listing tags is a read of `refs/tags`, which costs
//! milliseconds; knowing whether one exists on `origin` is a `ls-remote`, which
//! costs a network round trip. Claiming the second while only having done the
//! first is how a panel comes to say "pushed" about a tag nobody ever pushed —
//! so the list says nothing about the remote until it has been asked, and the
//! panel says which of the two it is showing.

use std::path::Path;

use anyhow::{bail, Result};

use super::{git, git_opt, git_reporting};

/// A tag as the panel lists it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Tag {
    pub name: String,
    /// Short hash of the **commit** it marks — never the tag object's own,
    /// which is what `objectname` gives for an annotated tag and what nothing
    /// on screen could be matched against.
    pub target: String,
    /// Annotated (a tag object of its own: author, date, message) rather than
    /// lightweight (a ref pointing straight at a commit). The distinction is
    /// worth showing: only an annotated tag carries a message, and only it is
    /// described by `git describe` by default.
    pub annotated: bool,
    /// Date the tag was created for an annotated one, of the commit for a
    /// lightweight one — relative, as git phrases it.
    pub date: String,
    /// The tag's message for an annotated one, the commit's subject otherwise.
    pub subject: String,
    /// Who tagged, or the commit's author for a lightweight tag.
    pub author: String,
}

/// The repository's tags, most recent first.
///
/// **By date and not by version order.** `--sort=-v:refname` reads well on a
/// project whose tags are all `v1.2.3`, and puts `hotfix-…` and `2026-08-14`
/// anywhere at all on the ones that are not; a date is true of every naming
/// scheme, and what one comes looking for in a review is what was tagged
/// lately.
pub fn list(main: &Path) -> Result<Vec<Tag>> {
    // `%00` is written literally by for-each-ref as a null byte, and a tag
    // message can contain anything else. The starred fields dereference an
    // annotated tag to the commit it points at, and are empty for a lightweight
    // one — which is what tells the two apart without a second command.
    const FORMAT: &str = "%(refname:short)%00%(objecttype)%00%(objectname:short)%00\
                          %(*objectname:short)%00%(creatordate:relative)%00\
                          %(contents:subject)%00%(taggername)%00%(authorname)";

    let raw = git(
        main,
        &[
            "for-each-ref",
            "--sort=-creatordate",
            &format!("--format={FORMAT}"),
            "refs/tags",
        ],
    )?;
    Ok(raw.lines().filter_map(parse).collect())
}

fn parse(line: &str) -> Option<Tag> {
    let mut f = line.split('\0');
    let name = f.next()?.to_string();
    if name.is_empty() {
        return None;
    }
    let kind = f.next().unwrap_or("");
    let object = f.next().unwrap_or("");
    let dereferenced = f.next().unwrap_or("");
    let date = f.next().unwrap_or("").to_string();
    let subject = f.next().unwrap_or("").to_string();
    let tagger = f.next().unwrap_or("");
    let author = f.next().unwrap_or("");

    let annotated = kind == "tag";
    Some(Tag {
        name,
        // An annotated tag's `objectname` is the tag object: what marks a
        // commit is the dereferenced one, and showing the other would give a
        // hash that appears nowhere in the history beside it.
        target: if dereferenced.is_empty() {
            object.to_string()
        } else {
            dereferenced.to_string()
        },
        annotated,
        date,
        subject,
        author: if tagger.is_empty() { author } else { tagger }.to_string(),
    })
}

/// Creates a tag, annotated when a message is given.
///
/// **The message decides the kind**, rather than a second switch beside it: an
/// annotated tag with no message is a prompt one has to fill in anyway, and a
/// lightweight tag with a message is not a thing git can make.
pub fn create(dir: &Path, name: &str, message: Option<&str>, at: Option<&str>) -> Result<()> {
    let name = check_name(name)?;
    let mut args: Vec<String> = vec!["tag".into()];
    match message.map(str::trim).filter(|m| !m.is_empty()) {
        Some(message) => {
            args.push("-a".into());
            args.push(name.to_string());
            args.push("-m".into());
            args.push(message.to_string());
        }
        None => args.push(name.to_string()),
    }
    args.extend(at.map(str::to_string));
    git(dir, &args).map(|_| ())
}

pub fn delete(main: &Path, name: &str) -> Result<()> {
    let name = check_name(name)?;
    git(main, &["tag", "-d", name]).map(|_| ())
}

/// Pushes one tag to `origin`.
///
/// `refs/tags/<name>` in full, never the bare name: a branch and a tag can
/// share a name, and git then pushes whichever it feels like — which is the one
/// case where a push does something other than what was asked.
pub fn push(dir: &Path, name: &str) -> Result<String> {
    let name = check_name(name)?;
    git_reporting(dir, &["push", "origin", &format!("refs/tags/{name}")])
}

/// Pushes every tag `origin` does not have.
pub fn push_all(dir: &Path) -> Result<String> {
    git_reporting(dir, &["push", "origin", "--tags"])
}

/// Removes a tag from `origin`, leaving the local one alone.
///
/// Deleting locally and deleting on the remote are **two gestures**, and this
/// module keeps them so: the panel offers both, and the second says plainly
/// what it does — a tag other people have pulled does not come back.
pub fn delete_remote(dir: &Path, name: &str) -> Result<String> {
    let name = check_name(name)?;
    git_reporting(
        dir,
        &["push", "origin", "--delete", &format!("refs/tags/{name}")],
    )
}

/// The tag names `origin` carries.
///
/// A network read, on purpose and on demand: it is the only way to know, and
/// paying a round trip every time a panel repaints is not a way to know
/// anything.
pub fn remote(dir: &Path) -> Result<Vec<String>> {
    let raw = git(dir, &["ls-remote", "--tags", "origin"])?;
    Ok(parse_remote(&raw))
}

/// `ls-remote` writes `<sha>\t refs/tags/<name>`, and an annotated tag twice —
/// once for the tag object, once for the commit under `^{}`. The second line is
/// the same tag, not another one.
fn parse_remote(raw: &str) -> Vec<String> {
    let mut names: Vec<String> = raw
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .filter_map(|reference| reference.strip_prefix("refs/tags/"))
        .map(|name| name.trim_end_matches("^{}").to_string())
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// Is a tag name one git will take?
///
/// Checked here rather than left to git, for one reason: the error git gives
/// (`fatal: 'v 1.0' is not a valid tag name`) arrives after the dialogue has
/// closed, where this one is shown under the field being typed. The rules are
/// `git check-ref-format`'s, cut down to what a tag name can break on.
pub fn is_valid_name(name: &str) -> bool {
    let name = name.trim();
    if name.is_empty() || name.starts_with('-') || name.starts_with('/') || name.ends_with('/') {
        return false;
    }
    if name.ends_with('.') || name.ends_with(".lock") || name.starts_with('.') {
        return false;
    }
    if name.contains("..") || name.contains("@{") || name.contains("//") {
        return false;
    }
    !name.chars().any(|c| {
        c.is_whitespace() || c.is_control() || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
    })
}

fn check_name(name: &str) -> Result<&str> {
    let trimmed = name.trim();
    if !is_valid_name(trimmed) {
        bail!("invalid tag name: {name}");
    }
    Ok(trimmed)
}

/// The tag describing a commit, if one does.
///
/// `git_opt` and not `git`: a repository with no tag at all answers with an
/// error, and that is the normal answer rather than a failure worth reporting.
pub fn describing(dir: &Path, commit: &str) -> Option<String> {
    git_opt(dir, &["describe", "--tags", "--exact-match", commit])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An annotated tag's own hash appears nowhere in the history beside it:
    /// what a row shows is the commit it marks.
    #[test]
    fn an_annotated_tag_reports_the_commit_it_marks() {
        let line = [
            "v1.2.0",
            "tag",
            "a1b2c3d",
            "9f8e7d6",
            "3 days ago",
            "Release 1.2.0",
            "Arno",
            "Someone",
        ]
        .join("\0");
        let tag = parse(&line).unwrap();
        assert_eq!(tag.name, "v1.2.0");
        assert!(tag.annotated);
        assert_eq!(tag.target, "9f8e7d6");
        assert_eq!(tag.subject, "Release 1.2.0");
        assert_eq!(tag.author, "Arno");
    }

    /// A lightweight tag has no tag object: no dereferenced hash, no tagger,
    /// and what it shows is the commit's own subject and author.
    #[test]
    fn a_lightweight_tag_falls_back_on_its_commit() {
        let line = [
            "nightly",
            "commit",
            "9f8e7d6",
            "",
            "2 hours ago",
            "Fix the thing",
            "",
            "Someone",
        ]
        .join("\0");
        let tag = parse(&line).unwrap();
        assert!(!tag.annotated);
        assert_eq!(tag.target, "9f8e7d6");
        assert_eq!(tag.author, "Someone");
    }

    /// A tag message can hold anything but a null byte — including the
    /// characters any other separator would have been.
    #[test]
    fn a_message_may_contain_anything() {
        let line = [
            "v2",
            "tag",
            "aaa",
            "bbb",
            "now",
            "Release: 2.0 | see #12",
            "Arno",
            "",
        ]
        .join("\0");
        assert_eq!(parse(&line).unwrap().subject, "Release: 2.0 | see #12");
    }

    /// `ls-remote` lists an annotated tag twice — the tag object, then the
    /// commit under `^{}` — and that is one tag, not two.
    #[test]
    fn a_remote_annotated_tag_is_listed_once() {
        let raw = "a1b2c3d\trefs/tags/v1.0\n9f8e7d6\trefs/tags/v1.0^{}\n1111111\trefs/tags/v0.9\n";
        assert_eq!(parse_remote(raw), ["v0.9", "v1.0"]);
    }

    #[test]
    fn an_empty_remote_lists_nothing() {
        assert!(parse_remote("").is_empty());
    }

    /// The error git would give arrives after the dialogue has closed; this one
    /// is shown under the field being typed.
    #[test]
    fn a_name_git_would_refuse_is_refused_here() {
        assert!(is_valid_name("v1.2.0"));
        assert!(is_valid_name("release/2026-08"));
        assert!(!is_valid_name(""));
        assert!(!is_valid_name("v 1.0"));
        assert!(!is_valid_name("v1..0"));
        assert!(!is_valid_name("v1.0.lock"));
        assert!(!is_valid_name("-v1"));
        assert!(!is_valid_name("v1^"));
        assert!(!is_valid_name("v1:0"));
        assert!(!is_valid_name("feature//x"));
        assert!(!is_valid_name(".hidden"));
    }
}
