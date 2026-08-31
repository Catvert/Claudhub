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
/// A snapshot taken when the list is built: the summary is read out of a table
/// of the application, and reading it from the virtualised closure would be a
/// borrow per row and per frame.
#[derive(Clone)]
pub(super) struct Item {
    pub main: PathBuf,
    pub path: PathBuf,
    pub label: String,
    pub branch: Option<String>,
    pub is_main: bool,
    pub summary: Option<crate::git::Summary>,
    /// What `wt` says: started, stopped, or nothing to start at all.
    pub up: Option<bool>,
    /// The rest of what `wt` says — options, ports, `[status.info]` — as the
    /// row's tooltip, when there is anything to say. See [`detail`].
    pub detail: Option<Detail>,
    /// Whether it has a button of its own in the top bar.
    pub pinned: bool,
}

/// The `wt` preview of a checkout: one part per line, nothing abbreviated.
///
/// It was **also** a condensed version, painted as a dim third line under the
/// branch, of which this was the tooltip. The line went — a picker answers
/// which checkout, not everything about it — and the abbreviation that cut a
/// list of twelve tenants down to `itcs, acme +10` went with it: there is no
/// width to fit any more, only a tooltip that can be as tall as it needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Detail {
    pub full: String,
}

/// What `wt` knows of a worktree beyond started/stopped, and in what order.
///
/// **The options first** — they are what one chose when creating it, which is
/// what tells two worktrees of the same branch apart — then the ports, then
/// the `[status.info]` lines, the project's own word on the worktree. `None`
/// when `wt` has nothing beyond started/stopped: the row then has no tooltip
/// rather than one saying nothing.
///
/// The values are **raw**: `wt` keeps the answers, not the labels of a
/// `[[prompt]]`, so it is `isolated` and not "isolées". Acceptable — they are
/// the project's own vocabulary, and the person who chose them reads them. A
/// yes/no answer shows its **name** when yes and nothing when no: a bare `no`
/// next to `isolated` names nothing, and `devtenant` alone says it.
pub(super) fn detail(state: &crate::runtime::protocol::WtWorktree) -> Option<Detail> {
    let mut parts: Vec<String> = Vec::new();
    for (name, value) in &state.opts {
        match yes_no(value) {
            Some(true) => parts.push(name.clone()),
            Some(false) => {}
            None => {
                let values = list(value);
                if values.is_empty() {
                    continue;
                }
                parts.push(format!("{name}: {}", values.join(", ")));
            }
        }
    }
    for (name, port) in &state.ports {
        parts.push(format!("{name}: {port}"));
    }
    for (name, value) in &state.info {
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        parts.push(format!("{name}: {value}"));
    }
    (!parts.is_empty()).then(|| Detail {
        full: parts.join("\n"),
    })
}

/// Reads an answer as a yes or a no, when it is one. The spellings `wt`'s
/// confirm prompts and the common hand-written ones produce; anything else
/// is a value in its own right.
fn yes_no(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "yes" | "y" | "true" | "on" | "1" => Some(true),
        "no" | "n" | "false" | "off" | "0" | "" => Some(false),
        _ => None,
    }
}

/// The entries of a list answer: a `multi` prompt joins them with a comma by
/// default, and the blanks around each are the shell's, not the value's.
fn list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect()
}

/// One line of the list.
#[derive(Clone)]
pub(super) enum Row {
    Repo {
        main: PathBuf,
        name: String,
        folded: bool,
        /// How many checkouts stand under it — what a folded repository has
        /// left to say: its name over nothing tells one the group is shut, not
        /// that eleven worktrees are behind it. The **filtered** count: the
        /// heading answers the list on screen, not the disk.
        count: usize,
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
/// `Item` — the expensive half, which asks the application a question or two per
/// checkout. A project with forty worktrees answered all of them to show two
/// rows.
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
            count: kept.len(),
        });
        if !folded {
            let mut built: Vec<Item> = kept
                .into_iter()
                .map(|checkout| item(repo, checkout))
                .collect();
            // **What is running comes first.** A project with a dozen checkouts
            // has two or three up at a time, and those are the ones one switches
            // between all day; the rest are read once, when one goes back to
            // them. It is the green dot the row already carries, sorted on.
            //
            // A **stable** sort, so that everything else keeps the order git
            // gave it — the main checkout first, then the linked ones as they
            // were created. And `up` is `None` where the project has no `wt` at
            // all: those rank with the stopped, which leaves such a repository's
            // list exactly as it was.
            built.sort_by_key(|item| !matches!(item.up, Some(true)));
            rows.extend(built.into_iter().map(Row::Worktree));
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
            up: None,
            detail: None,
            pinned: false,
        }
    }

    fn state(
        opts: &[(&str, &str)],
        ports: &[(&str, u16)],
        info: &[(&str, &str)],
    ) -> crate::runtime::protocol::WtWorktree {
        crate::runtime::protocol::WtWorktree {
            up: Some(true),
            endpoints: Vec::new(),
            branch: "wt/fix".into(),
            opts: opts
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ports: ports.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            info: info
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    /// The options first, the ports, then the project's `[status.info]`, one
    /// part per line — and an empty `[status.info]` value is not a part.
    #[test]
    fn the_detail_says_what_wt_knows_one_part_per_line() {
        let detail = detail(&state(
            &[
                ("db", "isolated"),
                ("devtenant", "no"),
                ("services", "queue,reverb"),
                ("tenants", "itcs, acme,xyz,abc"),
            ],
            &[("vite", 5201)],
            &[("bases", "wt_fix_tenant_\n"), ("centrale", "")],
        ))
        .unwrap();
        assert_eq!(
            detail.full,
            "db: isolated\nservices: queue, reverb\ntenants: itcs, acme, xyz, abc\nvite: 5201\nbases: wt_fix_tenant_"
        );
    }

    /// A yes/no answer shows its name when yes: `devtenant` alone says it,
    /// where a bare `yes` next to `isolated` would name nothing.
    #[test]
    fn a_yes_shows_its_name_and_a_no_nothing() {
        let detail = detail(&state(
            &[("devtenant", "yes"), ("verbose", "off")],
            &[],
            &[],
        ))
        .unwrap();
        assert_eq!(detail.full, "devtenant");
    }

    /// Nothing beyond started/stopped: the row then has no tooltip rather
    /// than one saying nothing.
    #[test]
    fn a_worktree_wt_knows_nothing_more_of_has_no_detail() {
        assert_eq!(detail(&state(&[], &[], &[("bases", "  ")])), None);
        assert_eq!(detail(&state(&[("devtenant", "no")], &[], &[])), None);
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

    /// A heading says how many checkouts stand under it, and it counts what the
    /// filter left: the number answers the list on screen, which is the only
    /// list one can see. Read on a folded repository, where it is all that is
    /// left of them.
    #[test]
    fn a_repository_counts_the_checkouts_the_filter_left() {
        let counts = |filter: &str| -> Vec<(String, usize)> {
            rows_for(&project(), &[], filter, &HashSet::new(), item)
                .into_iter()
                .filter_map(|row| match row {
                    Row::Repo { name, count, .. } => Some((name, count)),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(
            counts(""),
            vec![("acetics".to_string(), 2), ("claudhub".to_string(), 1)]
        );
        assert_eq!(counts("login"), vec![("acetics".to_string(), 1)]);
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

    /// The checkouts `wt` says are up come first, and the rest keep the order
    /// git gave them: a project runs two or three of a dozen at a time, and
    /// those are the ones one switches between all day.
    #[test]
    fn the_started_checkouts_are_listed_first() {
        let repos = vec![repo(
            "acetics",
            vec![
                checkout("main", "master"),
                checkout("fix", "fix/login"),
                checkout("db", "feat/db"),
                checkout("ui", "feat/ui"),
            ],
        )];
        let up = |_: &Repository, checkout: &Checkout| Item {
            up: Some(matches!(checkout.label.as_str(), "db" | "fix")),
            ..item(&repos[0], checkout)
        };
        let rows = rows_for(&repos, &[], "", &HashSet::new(), up);
        assert_eq!(names(&rows), ["== acetics", "fix", "db", "main", "ui"]);
    }

    /// A project without `wt` says nothing about anybody being up, and its list
    /// must come out exactly as git gave it.
    #[test]
    fn a_project_without_wt_keeps_its_order() {
        let rows = rows_for(&project(), &[], "", &HashSet::new(), item);
        assert_eq!(
            names(&rows),
            ["== acetics", "main", "fix", "== claudhub", "wip"]
        );
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
