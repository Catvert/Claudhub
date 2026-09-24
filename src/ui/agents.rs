//! What the agents say, as the window hears it: the balloons, and the hooks
//! put into a worktree.
//!
//! The decisions are not here — merging an agent's word with the processor it
//! burns, and when a word is news, are `agent::Tracker`'s; what the hooks
//! write and how they are installed is `agent_hooks`. This is the glue: the
//! sweep's two events in, a balloon or a command out.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use gpui_kit::{Context, SharedString};

use crate::agent::Activity;
use crate::agent_hooks::Record;
use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::settings::Settings;

impl ClaudhubApp {
    /// What the hooks last wrote, from the same sweep as `Evt::Agents`.
    ///
    /// **A balloon per transition, and only for a worktree one is not looking
    /// at**: the one on screen shows its badge in the title bar, and a balloon
    /// there would repeat it. The strip has no worktree on screen — it shows
    /// them all, and the column one would need may be scrolled away — so
    /// there every transition is said.
    pub(super) fn agent_sessions_heard(&mut self, sessions: Vec<Record>, cx: &mut Context<Self>) {
        self.terminal_sessions_heard(&sessions, cx);
        for told in self.agents.hear(sessions, Instant::now()) {
            if !self.overview && self.active.as_deref() == Some(told.worktree.as_path()) {
                continue;
            }
            let place = self.agent_place(&told.worktree);
            let text = match told.activity {
                Activity::Waiting(message) if message.is_empty() => {
                    tr!("agent-waiting-in-bare", { worktree: place })
                }
                Activity::Waiting(message) => {
                    tr!("agent-waiting-in", { worktree: place, message: message })
                }
                _ => tr!("agent-finished-in", { worktree: place }),
            };
            self.announce(text, cx);
        }
    }

    /// "repository · checkout", as the strip's heads write it.
    fn agent_place(&self, worktree: &Path) -> SharedString {
        match self.project_label(worktree) {
            (Some(repo), label) => SharedString::from(format!("{repo} · {label}")),
            (None, label) => label,
        }
    }

    /// Writes our hooks into a worktree, or takes them out.
    ///
    /// `asked` is a gesture's: its outcome is announced either way. An
    /// installation nobody asked for — a worktree that has just appeared —
    /// says something only when it fails.
    pub(super) fn set_agent_hooks(&mut self, worktree: PathBuf, install: bool, asked: bool) {
        if asked {
            self.agent_hooks_asked.insert(worktree.clone());
        }
        self.git.send(Cmd::AgentHooks { worktree, install });
    }

    pub(super) fn agent_hooks_written(
        &mut self,
        worktree: PathBuf,
        install: bool,
        result: Result<bool, String>,
        cx: &mut Context<Self>,
    ) {
        let asked = self.agent_hooks_asked.remove(&worktree);
        let place = self.agent_place(&worktree);
        match result {
            Ok(changed) if asked => {
                let key = match (install, changed) {
                    (true, true) => "agent-hooks-installed",
                    (true, false) => "agent-hooks-already",
                    (false, true) => "agent-hooks-removed",
                    (false, false) => "agent-hooks-absent",
                };
                self.announce(tr!(key, { worktree: place }), cx);
            }
            Ok(_) => {}
            Err(message) => {
                self.announce_error(
                    tr!("agent-hooks-failed", { worktree: place, message: message }),
                    cx,
                );
            }
        }
    }

    /// Puts our hooks in the worktrees that have just appeared in a
    /// repository's list, when the setting says so.
    ///
    /// **Appeared, not listed**: the first listing of a repository is what
    /// was there before Claudhub, and writing into every checkout one opens
    /// is not what the setting promises. What appears afterwards was created
    /// while the window watched — by the creation dialog, the branch picker, a
    /// pull request, or `wt new` in a terminal alongside: the same wish each
    /// time, an agent is about to be started there.
    pub(super) fn hook_new_worktrees(
        &mut self,
        before: Option<HashSet<PathBuf>>,
        after: &[PathBuf],
        cx: &Context<Self>,
    ) {
        if !Settings::global(cx).terminal.agent_hooks {
            return;
        }
        for worktree in appeared(before.as_ref(), after) {
            self.set_agent_hooks(worktree, true, false);
        }
    }
}

/// The checkouts of a new listing that the previous one did not have — none
/// when there was no previous one.
fn appeared(before: Option<&HashSet<PathBuf>>, after: &[PathBuf]) -> Vec<PathBuf> {
    let Some(before) = before else {
        return Vec::new();
    };
    after
        .iter()
        .filter(|path| !before.contains(*path))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_worktree_that_appears_gets_hooks() {
        let listed = |paths: &[&str]| paths.iter().map(PathBuf::from).collect::<Vec<_>>();
        // The first listing: what was there before us.
        assert!(appeared(None, &listed(&["/r", "/r-wt/a"])).is_empty());
        let before: HashSet<PathBuf> = listed(&["/r", "/r-wt/a"]).into_iter().collect();
        assert_eq!(
            appeared(Some(&before), &listed(&["/r", "/r-wt/a", "/r-wt/b"])),
            listed(&["/r-wt/b"])
        );
        // One removed: nothing to install.
        assert!(appeared(Some(&before), &listed(&["/r"])).is_empty());
    }
}
