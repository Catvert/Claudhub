//! What the worktree picker lists, and under which heading.
//!
//! The same split as `ui::branches` in front of `ui::branch_picker`: the two
//! rules the list actually has — a filter ignores the folds, a repository that
//! no longer opens is listed only when nothing is being filtered — lived in the
//! middle of a render, where neither could be tested and where breaking one
//! shows as a list that is merely a little wrong.

use std::collections::HashSet;
use std::path::PathBuf;

/// What the picker shows of one checkout.
///
/// A snapshot taken when the list is built: the summary and the agent are read
/// out of two tables of the application, and reading them from the virtualised
/// closure would be two borrows per row and per frame.
#[derive(Clone)]
pub(super) struct Item {
    pub main: PathBuf,
    pub path: PathBuf,
    pub label: String,
    pub branch: Option<String>,
    pub is_main: bool,
    pub summary: Option<crate::git::Summary>,
    pub agent: Option<crate::agent::State>,
    /// What `wt` says: started, stopped, or nothing to start at all.
    pub up: Option<bool>,
    /// Whether it has a button of its own in the top bar.
    pub pinned: bool,
}

/// One line of the list.
#[derive(Clone)]
pub(super) enum Row {
    Repo {
        main: PathBuf,
        name: String,
        folded: bool,
    },
    Worktree(Item),
    /// A remembered repository that no longer opens. It stays on the list
    /// because a repository that appears nowhere cannot be removed either.
    Missing {
        path: PathBuf,
        name: String,
        message: String,
    },
}

/// One checkout, as the filter judges it: the cheap half of a row.
///
/// The filter is answered on this, and only what it keeps is turned into an
/// `Item` — the expensive half, which asks the application three questions per
/// checkout. A project with forty worktrees answered all hundred and twenty of
/// them to show two rows.
pub(super) struct Checkout {
    pub path: PathBuf,
    pub label: String,
    pub branch: Option<String>,
    pub is_main: bool,
}

/// One repository and the checkouts it offers.
pub(super) struct Repository {
    pub main: PathBuf,
    pub name: String,
    pub checkouts: Vec<Checkout>,
}

/// A repository one has opened before and that no longer opens.
pub(super) struct Gone {
    pub path: PathBuf,
    pub name: String,
    pub message: String,
}

/// The rows on screen: each repository, then the checkouts of it that match.
///
/// A heading with nothing under it is dropped, as in the branch list: a title
/// followed by nothing reads as a display glitch rather than as a repository
/// where the filter found no match.
///
/// `item` fleshes out a checkout the filter kept. It is a closure because what
/// it needs — the summaries, the agents, `wt`'s state, the pins — belongs to the
/// application, and this decision does not.
pub(super) fn rows_for(
    repos: &[Repository],
    gone: &[Gone],
    filter: &str,
    folded: &HashSet<PathBuf>,
    mut item: impl FnMut(&Repository, &Checkout) -> Item,
) -> Vec<Row> {
    let needle = filter.trim().to_lowercase();
    let mut rows = Vec::new();
    for repo in repos {
        let kept: Vec<&Checkout> = repo
            .checkouts
            .iter()
            .filter(|checkout| keeps(checkout, &repo.name, &needle))
            .collect();
        if kept.is_empty() {
            continue;
        }
        // **A filter ignores the folds**, the window's rule for every foldable
        // list: a query that found something and shows nothing is read as a
        // query that found nothing.
        let folded = needle.is_empty() && folded.contains(&repo.main);
        rows.push(Row::Repo {
            main: repo.main.clone(),
            name: repo.name.clone(),
            folded,
        });
        if !folded {
            rows.extend(kept.into_iter().map(|checkout| {
                let built = item(repo, checkout);
                Row::Worktree(built)
            }));
        }
    }
    // Last, and only when nothing is being filtered: a folder that no longer
    // opens has no branch and no name to search on, and hiding it behind a
    // query would make it unremovable.
    if needle.is_empty() {
        rows.extend(gone.iter().map(|repo| Row::Missing {
            path: repo.path.clone(),
            name: repo.name.clone(),
            message: repo.message.clone(),
        }));
    }
    rows
}

/// Does a checkout answer the filter?
///
/// Its own name, its branch, or the repository's: one looks for a worktree by
/// the project it belongs to as often as by what it is called.
fn keeps(checkout: &Checkout, repo: &str, needle: &str) -> bool {
    needle.is_empty()
        || checkout.label.to_lowercase().contains(needle)
        || repo.to_lowercase().contains(needle)
        || checkout
            .branch
            .as_deref()
            .is_some_and(|branch| branch.to_lowercase().contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkout(label: &str, branch: &str) -> Checkout {
        Checkout {
            path: PathBuf::from(format!("/w/{label}")),
            label: label.into(),
            branch: (!branch.is_empty()).then(|| branch.to_string()),
            is_main: false,
        }
    }

    fn repo(name: &str, checkouts: Vec<Checkout>) -> Repository {
        Repository {
            main: PathBuf::from(format!("/repos/{name}")),
            name: name.into(),
            checkouts,
        }
    }

    fn item(repo: &Repository, checkout: &Checkout) -> Item {
        Item {
            main: repo.main.clone(),
            path: checkout.path.clone(),
            label: checkout.label.clone(),
            branch: checkout.branch.clone(),
            is_main: checkout.is_main,
            summary: None,
            agent: None,
            up: None,
            pinned: false,
        }
    }

    fn names(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                Row::Repo { name, folded, .. } => {
                    format!("== {name}{}", if *folded { " (fermé)" } else { "" })
                }
                Row::Worktree(item) => item.label.clone(),
                Row::Missing { name, .. } => format!("!! {name}"),
            })
            .collect()
    }

    fn project() -> Vec<Repository> {
        vec![
            repo(
                "acetics",
                vec![checkout("main", "master"), checkout("fix", "fix/login")],
            ),
            repo("claudhub", vec![checkout("wip", "feat/db")]),
        ]
    }

    fn gone() -> Vec<Gone> {
        vec![Gone {
            path: PathBuf::from("/repos/parti"),
            name: "parti".into(),
            message: "not a repository".into(),
        }]
    }

    #[test]
    fn every_repository_heads_its_checkouts() {
        let rows = rows_for(&project(), &[], "", &HashSet::new(), item);
        assert_eq!(
            names(&rows),
            ["== acetics", "main", "fix", "== claudhub", "wip"]
        );
    }

    /// A folded repository keeps its heading: that heading **is** the fold, and
    /// removing it too would leave no way back.
    #[test]
    fn a_folded_repository_keeps_its_heading_and_loses_its_checkouts() {
        let folded = HashSet::from([PathBuf::from("/repos/acetics")]);
        let rows = rows_for(&project(), &[], "", &folded, item);
        assert_eq!(names(&rows), ["== acetics (fermé)", "== claudhub", "wip"]);
    }

    /// **A filter ignores the folds.** A query that found something and shows
    /// nothing is read as a query that found nothing.
    #[test]
    fn a_filter_ignores_the_folds() {
        let folded = HashSet::from([PathBuf::from("/repos/acetics")]);
        let rows = rows_for(&project(), &[], "login", &folded, item);
        assert_eq!(names(&rows), ["== acetics", "fix"]);
    }

    /// A title followed by nothing reads as a display glitch rather than as a
    /// repository where the filter found no match.
    #[test]
    fn a_repository_the_filter_empties_loses_its_heading() {
        let rows = rows_for(&project(), &[], "wip", &HashSet::new(), item);
        assert_eq!(names(&rows), ["== claudhub", "wip"]);
    }

    /// The name of the checkout, its branch, or the repository's own: one looks
    /// for a worktree by the project it belongs to as often as by its name.
    #[test]
    fn the_filter_reads_the_checkout_its_branch_and_its_repository() {
        let matched = |needle| names(&rows_for(&project(), &[], needle, &HashSet::new(), item));
        assert_eq!(matched("FIX"), ["== acetics", "fix"]);
        assert_eq!(matched("master"), ["== acetics", "main"]);
        assert_eq!(matched("claud"), ["== claudhub", "wip"]);
        assert!(matched("rien").is_empty());
    }

    /// A folder that no longer opens has no branch and no name to search on,
    /// and hiding it behind a query would make it unremovable — so it is listed
    /// only when nothing is being filtered.
    #[test]
    fn the_repositories_that_no_longer_open_come_last_and_only_unfiltered() {
        let rows = rows_for(&project(), &gone(), "", &HashSet::new(), item);
        assert_eq!(names(&rows).last().unwrap(), "!! parti");
        let rows = rows_for(&project(), &gone(), "wip", &HashSet::new(), item);
        assert_eq!(names(&rows), ["== claudhub", "wip"]);
    }
}
