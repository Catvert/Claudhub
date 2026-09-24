//! The context sheet: what an agent working in a worktree can know of its
//! environment, written where it can read it (`$CLAUDHUB_CONTEXT`).
//!
//! **What the window knows and git does not say at once**: the base the
//! branch was guessed against, what the agents are doing, which terminals are
//! open, the nodes already on the home screen, the repository's other
//! worktrees. An agent can run `git` itself; it cannot ask this window.
//!
//! Plain Markdown, rewritten at every sweep and only when it changed (the
//! worker compares). Read-only for the agent — the skill says so.
//!
//! Pure: the view fills a `Sheet`, this writes it.

/// One worktree as the sheet speaks of it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Checkout {
    pub label: String,
    pub path: String,
    pub branch: Option<String>,
    /// Files touched, lines added and removed.
    pub changes: Option<(usize, usize, usize)>,
    /// What its agent is doing, in words: working, finished, waiting…
    pub agent: Option<String>,
}

/// A node already on the home screen.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeLine {
    pub kind: String,
    pub title: String,
    pub file: String,
    pub private: bool,
    pub author: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sheet {
    pub repo: String,
    pub here: Checkout,
    pub base: Option<String>,
    pub ahead_of_base: usize,
    /// Ahead and behind its upstream.
    pub upstream: Option<(usize, usize)>,
    /// Short id and subject, newest first.
    pub commits: Vec<(String, String)>,
    pub terminals: Vec<String>,
    pub nodes: Vec<NodeLine>,
    pub others: Vec<Checkout>,
}

fn changes(changes: Option<(usize, usize, usize)>) -> String {
    match changes {
        Some((files, added, removed)) if files > 0 => {
            format!("{files} file(s), +{added} −{removed}")
        }
        _ => "nothing to commit".into(),
    }
}

/// The sheet as Markdown. English, like everything an agent is given.
pub fn render(sheet: &Sheet) -> String {
    let mut out = format!(
        "# {} · {}\n\n_Written by Claudhub, rewritten as things change. Read it; do not edit it._\n\n",
        sheet.repo, sheet.here.label
    );
    out.push_str(&format!("- Worktree: `{}`\n", sheet.here.path));
    out.push_str(&format!(
        "- Branch: {}\n",
        sheet
            .here
            .branch
            .as_deref()
            .map_or("detached HEAD".into(), |b| format!("`{b}`"))
    ));
    if let Some(base) = &sheet.base {
        out.push_str(&format!(
            "- Base: `{base}` — {} commit(s) ahead\n",
            sheet.ahead_of_base
        ));
    }
    if let Some((ahead, behind)) = sheet.upstream {
        out.push_str(&format!("- Upstream: {ahead} ahead, {behind} behind\n"));
    }
    out.push_str(&format!("- Uncommitted: {}\n", changes(sheet.here.changes)));
    if let Some(agent) = &sheet.here.agent {
        out.push_str(&format!("- Agent: {agent}\n"));
    }
    if !sheet.commits.is_empty() {
        out.push_str("\n## Latest commits\n\n");
        for (short, subject) in &sheet.commits {
            out.push_str(&format!("- `{short}` {subject}\n"));
        }
    }
    if !sheet.terminals.is_empty() {
        out.push_str("\n## Open terminals\n\n");
        for terminal in &sheet.terminals {
            out.push_str(&format!("- {terminal}\n"));
        }
    }
    out.push_str("\n## Nodes on the home screen\n\n");
    if sheet.nodes.is_empty() {
        out.push_str("None yet.\n");
    }
    for node in &sheet.nodes {
        out.push_str(&format!(
            "- {} — {} (`{}`{}{})\n",
            node.kind,
            node.title,
            node.file,
            if node.private { ", private" } else { "" },
            node.author
                .as_deref()
                .map(|a| format!(", by {a}"))
                .unwrap_or_default()
        ));
    }
    if !sheet.others.is_empty() {
        out.push_str("\n## Other worktrees of the repository\n\n");
        for other in &sheet.others {
            out.push_str(&format!(
                "- {} (`{}`) — {}, {}{}\n",
                other.label,
                other.path,
                other.branch.as_deref().unwrap_or("detached HEAD"),
                changes(other.changes),
                other
                    .agent
                    .as_deref()
                    .map(|a| format!(", agent {a}"))
                    .unwrap_or_default()
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sheet_says_where_the_branch_stands_and_what_is_around() {
        let sheet = Sheet {
            repo: "acme".into(),
            here: Checkout {
                label: "feature-x".into(),
                path: "/r/feature-x".into(),
                branch: Some("feature/x".into()),
                changes: Some((3, 40, 2)),
                agent: Some("working".into()),
            },
            base: Some("main".into()),
            ahead_of_base: 2,
            upstream: Some((1, 0)),
            commits: vec![("abc123".into(), "Cache the rates".into())],
            terminals: vec!["claude (agent)".into()],
            nodes: vec![NodeLine {
                kind: "review".into(),
                title: "Review: feature/x".into(),
                file: ".claudhub/notes/2026-09-24-2130-review.md".into(),
                private: false,
                author: Some("Ada".into()),
            }],
            others: vec![Checkout {
                label: "acme".into(),
                path: "/r".into(),
                branch: Some("main".into()),
                changes: None,
                agent: None,
            }],
        };
        let text = render(&sheet);
        assert!(text.starts_with("# acme · feature-x\n"));
        assert!(text.contains("- Base: `main` — 2 commit(s) ahead\n"));
        assert!(text.contains("- Uncommitted: 3 file(s), +40 −2\n"));
        assert!(text.contains("- `abc123` Cache the rates\n"));
        assert!(text.contains(
            "- review — Review: feature/x (`.claudhub/notes/2026-09-24-2130-review.md`, by Ada)\n"
        ));
        assert!(text.contains("- acme (`/r`) — main, nothing to commit\n"));
    }

    #[test]
    fn an_empty_home_screen_says_so() {
        assert!(render(&Sheet::default()).contains("## Nodes on the home screen\n\nNone yet.\n"));
    }
}
