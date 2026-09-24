//! What a worktree's card on the home screen says of its branch: where it
//! started, what it has added since, and how far it is from its upstream.
//!
//! A reading of its own and not the history's: the history lays out a graph of
//! two thousand commits for the checkout on screen, and this runs for every
//! open worktree at once, for the handful of lines a card has room for.

use std::path::Path;

use anyhow::Result;

use super::{branch, git, git_opt, split_nul};

/// One commit, as a card lists it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CommitLine {
    pub short: String,
    pub subject: String,
    /// Committer time, in seconds since the epoch: the card says "3 h ago",
    /// and git's own relative phrase is English.
    pub at: i64,
}

/// What a card knows of a worktree's branch.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Outline {
    /// The branch this one most likely started from — `branch::guess_base`.
    pub base: Option<String>,
    /// Commits reachable from HEAD and not from the base.
    pub ahead_of_base: usize,
    /// The newest of those, or, with none, the newest of HEAD: a main checkout
    /// is its own base, and what a card says of it is what landed last.
    pub commits: Vec<CommitLine>,
    /// Ahead and behind the upstream; `None` without one.
    pub upstream: Option<(usize, usize)>,
}

/// Reads a worktree's outline: four commands, none of which reads a file.
pub fn outline(dir: &Path, limit: usize) -> Result<Outline> {
    let base = branch::guess_base(dir);
    let ahead_of_base = base
        .as_deref()
        .and_then(|base| git_opt(dir, &["rev-list", "--count", &format!("{base}..HEAD")]))
        .and_then(|count| count.trim().parse().ok())
        .unwrap_or(0);
    let mut args = vec![
        "log".to_string(),
        "-z".to_string(),
        "--no-show-signature".to_string(),
        "--format=%h%x1f%ct%x1f%s".to_string(),
        format!("--max-count={limit}"),
    ];
    match base.as_deref() {
        Some(base) if ahead_of_base > 0 => args.push(format!("{base}..HEAD")),
        _ => args.push("HEAD".to_string()),
    }
    // A repository without a commit has no HEAD to log: an empty list.
    let commits = git(dir, &args)
        .map(|out| parse_log(&out))
        .unwrap_or_default();
    let upstream = git_opt(
        dir,
        &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"],
    )
    .and_then(|out| parse_counts(&out));
    Ok(Outline {
        base,
        ahead_of_base,
        commits,
        upstream,
    })
}

/// `%h\x1f%ct\x1f%s`, one record per NUL.
fn parse_log(out: &str) -> Vec<CommitLine> {
    split_nul(out)
        .filter_map(|record| {
            let mut fields = record.trim_start_matches('\n').splitn(3, '\x1f');
            let short = fields.next()?.to_string();
            let at = fields.next()?.parse().ok()?;
            let subject = fields.next()?.trim().to_string();
            (!short.is_empty()).then_some(CommitLine { short, subject, at })
        })
        .collect()
}

/// `rev-list --left-right --count` answers "ahead\tbehind".
fn parse_counts(out: &str) -> Option<(usize, usize)> {
    let mut parts = out.split_whitespace();
    let ahead = parts.next()?.parse().ok()?;
    let behind = parts.next()?.parse().ok()?;
    Some((ahead, behind))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_record_is_its_short_id_its_time_and_its_subject() {
        let out = "abc1234\x1f1700000000\x1fFix: a | b\x1f c\0def5678\x1f1690000000\x1fStart\0";
        assert_eq!(
            parse_log(out),
            vec![
                CommitLine {
                    short: "abc1234".into(),
                    subject: "Fix: a | b\x1f c".into(),
                    at: 1_700_000_000,
                },
                CommitLine {
                    short: "def5678".into(),
                    subject: "Start".into(),
                    at: 1_690_000_000,
                },
            ]
        );
        // A torn record says nothing rather than something false.
        assert!(parse_log("abc\x1fnot a time\x1fx\0").is_empty());
    }

    #[test]
    fn the_upstream_counts_are_ahead_then_behind() {
        assert_eq!(parse_counts("2\t5\n"), Some((2, 5)));
        assert_eq!(parse_counts(""), None);
    }

    /// A branch two commits past its base lists those two; the base lists
    /// its own last commits, having nothing ahead of itself.
    #[test]
    fn a_branch_lists_what_it_added_and_the_base_what_landed_last() {
        let root = std::env::temp_dir().join(format!("claudhub-outline-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let run = |args: &[&str]| git(&root, args).unwrap();
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "T"]);
        run(&["config", "commit.gpgsign", "false"]);
        run(&["commit", "-q", "--allow-empty", "-m", "Start"]);
        run(&["switch", "-q", "-c", "feature"]);
        run(&["commit", "-q", "--allow-empty", "-m", "One"]);
        run(&["commit", "-q", "--allow-empty", "-m", "Two"]);

        let feature = outline(&root, 5).unwrap();
        assert_eq!(feature.base.as_deref(), Some("main"));
        assert_eq!(feature.ahead_of_base, 2);
        let subjects: Vec<_> = feature.commits.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects, ["Two", "One"]);
        assert_eq!(feature.upstream, None);

        run(&["switch", "-q", "main"]);
        let main = outline(&root, 5).unwrap();
        assert_eq!(main.ahead_of_base, 0);
        assert_eq!(
            main.commits.first().map(|c| c.subject.as_str()),
            Some("Start")
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
