//! The multiplexer: every terminal of the window, at once.
//!
//! The terminals are a dock panel, a dock panel belongs to one area at a time,
//! and every area only ever shows the worktree being looked at. Whatever runs
//! in the eleven others is therefore alive and invisible — which is exactly the
//! state one leaves half a dozen agents in. The question that has no view is
//! "which of them has finished", and it is the only question that crosses the
//! worktrees.
//!
//! **So this screen is not a view at all: it is a dock with nothing in it but
//! the terminals.** A terminal already has one face per screen — the same
//! `Entity<TerminalView>`, one panel each — and the multiplexer's face differs
//! in exactly two ways (`Workspace::shows_every_worktree`): it shows itself
//! whatever worktree is being looked at, and its tab says which project it
//! belongs to. Everything else — splits, resize handles, tabs one drags, zoom,
//! the close cross, the rename — is the dock's, unchanged.
//!
//! That is what is left of two answers that came before. A **grid of tiles**
//! painted by us was a picture of a dock, with none of what a dock does. **One
//! area per project**, stacked under headings, made the boundary between
//! projects something the layout enforced — at the price of a page nothing
//! could be rearranged across, of a drag between two areas that invited a drop
//! it then refused, and of a panel of ours whose tab bar sat above the
//! terminals' own, saying what the title bar's button had already said. What
//! tells the projects apart is the **tab**, which is where one is already
//! reading the terminal's name; once it says so, mixing them costs nothing and
//! buys the only thing this screen is for, which is arranging them side by side
//! by hand.
//!
//! Two consequences worth knowing:
//!
//! - **The terminals are live, and it costs a resize.** They are the very views
//!   the other screens show, so one types in them; and a `TerminalView` sizes
//!   its pty to the room it is given, so entering this screen resizes every
//!   terminal on it and leaving puts them back. That is what tmux does when a
//!   pane is zoomed. The pty is only told **once the drag stops** — see
//!   `TerminalView::request_size` — and the badge it paints meanwhile says what
//!   geometry is coming.
//! - **No pty is ever drawn twice in a frame**, which is what makes the faces
//!   safe: only one dock is on screen at a time.
//!
//! All that is left here is how a project is named in a tab.

use std::path::Path;

use gpui::SharedString;

use crate::ui::app::ClaudhubApp;

impl ClaudhubApp {
    /// How a worktree is named where there is no room for a path: the
    /// repository, then the worktree, the way the picker writes it.
    ///
    /// The two collapse into one when they say the same thing — a main checkout
    /// is usually a folder named after its repository, and "nixos / nixos" is a
    /// word wasted on a tab bar. Two worktrees called `main` in two
    /// repositories is the case this exists for, and it is precisely the case
    /// where they differ.
    pub(super) fn project_label(&self, worktree: &Path) -> (Option<SharedString>, SharedString) {
        let repo = self
            .repo_of(worktree)
            .map(|repo| SharedString::from(repo.name.clone()));
        let label = SharedString::from(
            self.repos
                .worktree(worktree)
                .map(|found| found.label())
                // A worktree that went away while its terminals were still
                // running: the path's last segment is what is recognised, and
                // it is what the picker shows too.
                .unwrap_or_else(|| {
                    worktree
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| worktree.display().to_string())
                }),
        );
        match repo {
            Some(repo) if repo == label => (None, label),
            repo => (repo, label),
        }
    }
}
