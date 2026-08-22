//! Which databases of a connection belong to the checkout being looked at.
//!
//! A project like Acetics clones its databases per worktree: `wt new telavox`
//! leaves `wt_telavox_master` and a `wt_telavox_tenant_*` beside the eighty
//! databases of the main repository. The tree showed all of them, which is the
//! same as showing none — the three that belong to the branch under review are
//! lost among the rest.
//!
//! **A pattern declared on the connection**, second level of the extension
//! system: `wt_{slug}_*` says it once and for every worktree. It is not a
//! `wt.toml` matter — a connection belongs to the machine, not to the project,
//! and the same repository is reviewed from five checkouts against the same
//! server.
//!
//! Three rules, and each of them is what keeps the thing honest:
//!
//! - **A pattern whose variable does not resolve is dropped.** The main
//!   checkout has no slug: `wt_{slug}_*` says nothing there, and guessing an
//!   empty string would turn it into `wt__*`, which matches nothing at all.
//! - **No applicable pattern shows everything.** A scope that filters nothing
//!   is a scope that never hides what it does not know about — the main
//!   repository, a connection with no pattern, a checkout not yet known.
//! - **Nothing is hidden silently.** The count of what a scope removes is part
//!   of what this module returns, because the panel has to say it.

/// What the checkout being looked at is called, as a pattern names it.
///
/// Each field is optional and its absence is meaningful: it is what drops a
/// pattern rather than resolving it to nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Vars {
    /// The checkout's folder name — always known once a worktree is selected.
    pub worktree: Option<String>,
    /// The `wt` slug, which is the linked worktree's folder name. **`None` on
    /// the main checkout**, which is what makes `wt_{slug}_*` inert there.
    pub slug: Option<String>,
    /// The checked-out branch, `None` on a detached HEAD.
    pub branch: Option<String>,
}

impl Vars {
    fn get(&self, name: &str) -> Option<&str> {
        match name {
            "worktree" => self.worktree.as_deref(),
            "slug" => self.slug.as_deref(),
            "branch" => self.branch.as_deref(),
            _ => None,
        }
    }

    /// The names a pattern may use, for the settings form's help.
    pub const NAMES: [&'static str; 3] = ["worktree", "slug", "branch"];
}

/// Turns what the connection declares into the patterns that apply here.
///
/// Patterns are separated by commas or newlines — a comma because the field is
/// one line in the form, a newline because a hand-edited `settings.json` writes
/// lists that way.
pub fn expand(declared: &str, vars: &Vars) -> Vec<String> {
    declared
        .split([',', '\n'])
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
        .filter_map(|pattern| substitute(pattern, vars))
        .collect()
}

/// Replaces `{name}` by its value, or gives up on the whole pattern.
///
/// Giving up is the point: a pattern that mentions something this checkout does
/// not have is not about this checkout.
fn substitute(pattern: &str, vars: &Vars) -> Option<String> {
    let mut out = String::with_capacity(pattern.len());
    let mut rest = pattern;
    while let Some(start) = rest.find('{') {
        let end = rest[start..].find('}')? + start;
        out.push_str(&rest[..start]);
        out.push_str(vars.get(&rest[start + 1..end])?);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// Does a name match one pattern?
///
/// `*` stands for any run of characters, `?` for exactly one, and the
/// comparison ignores case — database names are case-insensitive on MySQL under
/// Windows and half of them are written by a migration nobody reads.
pub fn matches(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.to_lowercase().chars().collect();
    let name: Vec<char> = name.to_lowercase().chars().collect();
    glob(&pattern, &name)
}

/// The classic backtracking walk: linear as long as there is one `*`, and the
/// names here are twenty characters long.
fn glob(pattern: &[char], name: &[char]) -> bool {
    let (mut p, mut n) = (0, 0);
    // Where to resume when a `*` has to swallow one more character.
    let (mut star, mut resume) = (None, 0);
    while n < name.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some(p);
                resume = n;
                p += 1;
            }
            Some('?') => {
                p += 1;
                n += 1;
            }
            Some(c) if *c == name[n] => {
                p += 1;
                n += 1;
            }
            _ => match star {
                Some(index) => {
                    p = index + 1;
                    resume += 1;
                    n = resume;
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

/// Does this database belong to the checkout?
///
/// **No pattern means everything belongs**: a scope that cannot decide must not
/// be the reason a database disappears.
pub fn allows(patterns: &[String], name: &str) -> bool {
    patterns.is_empty() || patterns.iter().any(|pattern| matches(pattern, name))
}

/// Splits a list of names into what the scope keeps and how much it hides.
///
/// The count comes back with the list because the panel says it: a filter that
/// removes seventy-eight databases without a word reads as a broken connection.
pub fn split<'a, T>(
    patterns: &[String],
    items: impl IntoIterator<Item = &'a T>,
    name: impl Fn(&T) -> &str,
) -> (Vec<&'a T>, usize)
where
    T: 'a,
{
    let mut kept = Vec::new();
    let mut hidden = 0;
    for item in items {
        if allows(patterns, name(item)) {
            kept.push(item);
        } else {
            hidden += 1;
        }
    }
    (kept, hidden)
}

/// The example the settings form shows, and what it is worth: a project whose
/// worktrees clone their databases writes exactly this.
pub const EXAMPLE: &str = "wt_{slug}_*";

#[cfg(test)]
mod tests {
    use super::*;

    fn worktree_vars() -> Vars {
        Vars {
            worktree: Some("telavox".into()),
            slug: Some("telavox".into()),
            branch: Some("wt/telavox".into()),
        }
    }

    fn main_vars() -> Vars {
        Vars {
            worktree: Some("Acetics".into()),
            slug: None,
            branch: Some("master".into()),
        }
    }

    #[test]
    fn a_pattern_is_resolved_against_the_checkout() {
        assert_eq!(
            expand("wt_{slug}_*", &worktree_vars()),
            vec!["wt_telavox_*".to_string()]
        );
    }

    /// The main checkout has no slug, and a pattern that names one is not about
    /// it: dropped rather than resolved to `wt__*`, which would match nothing
    /// and empty the tree.
    #[test]
    fn a_pattern_naming_an_unknown_variable_is_dropped() {
        assert!(expand("wt_{slug}_*", &main_vars()).is_empty());
        assert!(expand("{nothing}_*", &worktree_vars()).is_empty());
    }

    #[test]
    fn several_patterns_are_separated_by_commas_or_lines() {
        let patterns = expand("wt_{slug}_* , shared\nlogs_{branch}", &worktree_vars());
        assert_eq!(patterns, ["wt_telavox_*", "shared", "logs_wt/telavox"]);
    }

    #[test]
    fn a_star_matches_a_run_and_a_question_mark_one_character() {
        assert!(matches("wt_telavox_*", "wt_telavox_tenant_itcs"));
        assert!(matches("wt_telavox_*", "wt_telavox_"));
        assert!(!matches("wt_telavox_*", "wt_other_master"));
        assert!(matches("tenant_?", "tenant_1"));
        assert!(!matches("tenant_?", "tenant_12"));
        assert!(matches("*_master", "wt_telavox_master"));
        assert!(matches("*", "anything"));
    }

    /// A name written by a migration nobody reads is not always in the case one
    /// expects, and MySQL does not always care either.
    #[test]
    fn matching_ignores_case() {
        assert!(matches("WT_TELAVOX_*", "wt_telavox_master"));
        assert!(matches("wt_telavox_*", "WT_TELAVOX_MASTER"));
    }

    /// The rule that keeps a scope from ever being the reason something
    /// disappears without a word.
    #[test]
    fn nothing_is_filtered_when_no_pattern_applies() {
        assert!(allows(&[], "acetics_master"));
        let names = ["a".to_string(), "b".to_string()];
        let (kept, hidden) = split(&[], names.iter(), |name: &String| name.as_str());
        assert_eq!(kept.len(), 2);
        assert_eq!(hidden, 0);
    }

    #[test]
    fn what_is_hidden_is_counted() {
        let names = [
            "acetics_master".to_string(),
            "wt_telavox_master".to_string(),
            "wt_telavox_tenant_itcs".to_string(),
        ];
        let patterns = expand("wt_{slug}_*", &worktree_vars());
        let (kept, hidden) = split(&patterns, names.iter(), |name| name.as_str());
        assert_eq!(kept, ["wt_telavox_master", "wt_telavox_tenant_itcs"]);
        assert_eq!(hidden, 1);
    }
}
