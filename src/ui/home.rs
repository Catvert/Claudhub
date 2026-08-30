//! The home page: what the centre shows when it holds nothing else.
//!
//! It took the place of the "pick a file" placeholder, which stood for the
//! whole centre and said nothing about the project one had just opened. What it
//! says is what one looks for on arriving: which checkout this is, where the
//! branch stands against its upstream and its base, what is waiting to be
//! committed, and what the other worktrees are doing.
//!
//! **Nothing here asks git for anything.** Every figure is read off state the
//! window already holds — the status the watcher keeps fresh, the branches read
//! once per repository, the summaries swept every ten seconds, the `justfile`
//! and `wt` read in the background. The rule the panels live by is that a read
//! is paid for by the panel being painted (`ensure_history`, `ensure_sentry`);
//! this one is painted at every start, which makes it exactly the tab that must
//! not pay for a command.
//!
//! **Its tab wears the house alone**, a pinned tab's shape (`panels::
//! pinned_glyph`): it is not a document one has opened, it is the room one
//! comes back to. And it carries no cross — what one does with it is open
//! something else, which is exactly what makes it step aside.

use std::path::PathBuf;

use gpui::{div, prelude::*, px, AnyElement, App, Context, SharedString, Window};
use gpui_component::{h_flex, v_flex, ActiveTheme, Sizable as _, StyledExt as _};

use crate::git::Summary;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::settings::Settings;

/// What one row of the worktree list says.
///
/// A snapshot taken while the page is built: the closures that follow cannot
/// read the application back, and the summary lives in a table of its own.
struct Checkout {
    path: PathBuf,
    label: String,
    branch: Option<String>,
    summary: Option<Summary>,
    /// An agent is running there, and whether it is doing anything.
    agent: Option<bool>,
    here: bool,
}

/// What the status has to say in figures, read before the page is built: what
/// paints it takes `&mut self`, and the status is borrowed from the same place.
struct Counts {
    staged: usize,
    unstaged: usize,
    conflicted: usize,
}

/// Past this many checkouts the list is cut and the rest counted: the card is a
/// glance at the project, not the worktree picker.
const MAX_CHECKOUTS: usize = 6;

impl ClaudhubApp {
    /// Whether the home tab is on screen.
    ///
    /// It stands for the centre only while the centre has nothing else: as soon
    /// as a file, a diff, a query or a hit arrives it would be a tab beside
    /// them, and a home page among four documents is one of the empty rooms the
    /// `needed:` rule exists to remove. That is also all there is to putting it
    /// away — it carries no cross, and opening anything is what closes it.
    pub(super) fn home_visible(&self) -> bool {
        self.panel_visible(crate::ui::panels::EditorPanel::NAME)
            && !self.diff_on_screen()
            && !self.db_console_open()
            && self
                .editing_root()
                .and_then(|root| self.editings.get(&root))
                .is_none_or(|tabs| tabs.open.is_empty())
    }

    pub(super) fn render_home(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(worktree) = self.active.clone() else {
            return home_note(tr!("no-worktree"), cx);
        };
        let theme = cx.theme();
        let (muted, border) = (theme.muted_foreground, theme.border);
        let mono = theme.mono_font_family.clone();

        let repo = self.repo_of(&worktree);
        let project: SharedString = repo
            .map(|repo| repo.name.clone())
            .unwrap_or_else(|| worktree.display().to_string())
            .into();
        // The branch as the checkout knows it, and the row `for-each-ref` gave
        // for it: the first is always there, the second carries the last commit
        // and the upstream and arrives one command later.
        let head = self
            .active_worktree()
            .and_then(|worktree| worktree.branch.clone());
        let branch = repo
            .and_then(|repo| repo.branches.iter().find(|b| b.is_head_in(&worktree)))
            .cloned();
        let checkouts: Vec<Checkout> = repo
            .map(|repo| {
                repo.worktrees
                    .iter()
                    .map(|tree| Checkout {
                        label: tree.label(),
                        branch: tree.branch.clone(),
                        summary: self.summaries.get(&tree.path).cloned(),
                        agent: self.agents.get(&tree.path).map(|state| state.working),
                        here: tree.path == worktree,
                        path: tree.path.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();

        let review = self.review.get(&worktree);
        // The counts are taken here rather than a borrow of the status being
        // handed down: what follows takes `&mut self`.
        let status = review.map(|review| &review.status);
        let counts = Counts {
            staged: status.map(|status| status.staged().count()).unwrap_or(0),
            unstaged: status.map(|status| status.unstaged().count()).unwrap_or(0),
            conflicted: status
                .map(|status| status.conflicted().count())
                .unwrap_or(0),
        };
        let base = review.and_then(|review| review.base.clone());
        let lsp = review.is_some_and(|review| review.lsp);
        let summary = self.summaries.get(&worktree).cloned();
        let recipes = self
            .just_recipes
            .get(&worktree)
            .and_then(|snapshot| snapshot.as_ref())
            .map(|snapshot| snapshot.recipes.len())
            .filter(|count| *count > 0);
        let up = self.wt_states.get(&worktree).and_then(|state| state.up);

        v_flex()
            .id("home")
            .size_full()
            .overflow_y_scroll()
            .p_6()
            .items_center()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(880.))
                    .gap_4()
                    // ── Who one is looking at ───────────────────────────────
                    .child(
                        h_flex()
                            .w_full()
                            .gap_3()
                            .items_center()
                            .child(icon("house").large().text_color(muted.opacity(0.7)))
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .child(div().text_xl().font_semibold().child(project))
                                    .child(
                                        div()
                                            .truncate()
                                            .text_xs()
                                            .font_family(mono)
                                            .text_color(muted)
                                            .child(worktree.display().to_string()),
                                    ),
                            ),
                    )
                    .child(div().w_full().flex_shrink_0().h(px(1.)).bg(border))
                    // ── The two cards one comes for ─────────────────────────
                    .child(
                        h_flex()
                            .w_full()
                            .gap_4()
                            .flex_wrap()
                            .items_start()
                            .child(self.render_home_branch(head, branch, base, cx))
                            .child(self.render_home_changes(counts, summary, cx)),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .gap_4()
                            .flex_wrap()
                            .items_start()
                            .child(self.render_home_checkouts(checkouts, cx))
                            .child(render_home_project(recipes, up, lsp, cx)),
                    )
                    .child(render_home_keys(cx)),
            )
            .into_any_element()
    }

    /// Where the branch stands: its upstream, its base, its last commit.
    fn render_home_branch(
        &mut self,
        head: Option<String>,
        branch: Option<crate::git::Branch>,
        base: Option<String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (muted, success, warning) = (theme.muted_foreground, theme.success, theme.warning);
        let name: SharedString = head
            .map(SharedString::from)
            .unwrap_or_else(|| tr!("branch-detached"));
        let upstream = branch.as_ref().and_then(|branch| branch.upstream.clone());
        let last = branch.map(|branch| (branch.subject, branch.author, branch.date));
        card("home-branch", "git-branch", cx)
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .flex_wrap()
                    .items_center()
                    .child(div().min_w_0().font_semibold().truncate().child(name))
                    .when_some(upstream.clone(), |el, upstream| {
                        el.child(chip(
                            "arrow-up",
                            format!("{}", upstream.ahead).into(),
                            if upstream.ahead > 0 { success } else { muted },
                        ))
                        .child(chip(
                            "arrow-down",
                            format!("{}", upstream.behind).into(),
                            if upstream.behind > 0 { warning } else { muted },
                        ))
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .truncate()
                    .child(match upstream {
                        Some(upstream) => tr!("home-upstream", { name: upstream.name }),
                        None => tr!("home-upstream-none"),
                    }),
            )
            .when_some(base, |el, base| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .truncate()
                        .child(tr!("home-base", { name: base })),
                )
            })
            .when_some(last, |el, (subject, author, date)| {
                el.child(
                    v_flex()
                        .w_full()
                        .gap_0p5()
                        .child(div().text_sm().truncate().child(subject))
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .truncate()
                                .child(format!("{author} · {date}")),
                        ),
                )
            })
            .into_any_element()
    }

    /// What is waiting to be committed, and the way to it.
    fn render_home_changes(
        &mut self,
        counts: Counts,
        summary: Option<Summary>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (muted, success, danger, warning) = (
            theme.muted_foreground,
            theme.success,
            theme.danger,
            theme.warning,
        );
        let Counts {
            staged,
            unstaged,
            conflicted,
        } = counts;
        let files = summary.as_ref().map(|summary| summary.files).unwrap_or(0);
        let clean = files == 0 && staged == 0 && unstaged == 0;
        card("range-working", "file-diff", cx)
            .when(clean, |el| {
                el.child(div().text_sm().text_color(muted).child(tr!("home-clean")))
            })
            .when(!clean, |el| {
                el.child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .flex_wrap()
                        .items_center()
                        .child(
                            div()
                                .font_semibold()
                                .child(tr!("home-files", { count: files })),
                        )
                        .when_some(summary, |el, summary| {
                            el.child(chip("plus", format!("{}", summary.added).into(), success))
                                .child(chip("minus", format!("{}", summary.removed).into(), danger))
                        }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(tr!("home-staged", { staged: staged, unstaged: unstaged })),
                )
                .when(conflicted > 0, |el| {
                    el.child(
                        div()
                            .text_xs()
                            .text_color(warning)
                            .child(tr!("home-conflicted", { count: conflicted })),
                    )
                })
            })
            // The card is the way to the list it counts: what one does after
            // reading "twenty-three files" is look at them. Nothing to look at
            // on a clean checkout, so nothing to press either.
            .when(!clean, |el| {
                el.child(link(
                    "home-open-changes",
                    tr!("home-open-changes"),
                    cx.listener(|this, _, window, cx| {
                        this.reveal_panel(crate::ui::panels::ChangesPanel::NAME, window, cx)
                    }),
                    cx,
                ))
            })
            .into_any_element()
    }

    /// The repository's other checkouts, and what is happening in them.
    fn render_home_checkouts(
        &mut self,
        checkouts: Vec<Checkout>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (muted, accent, success) = (
            theme.muted_foreground,
            theme.accent_foreground,
            theme.success,
        );
        let hovered = theme.secondary;
        let total = checkouts.len();
        let shown: Vec<Checkout> = checkouts.into_iter().take(MAX_CHECKOUTS).collect();
        let rest = total.saturating_sub(shown.len());
        card("home-checkouts", "gallery-vertical-end", cx)
            .children(shown.into_iter().enumerate().map(|(rank, checkout)| {
                let path = checkout.path.clone();
                let dirty = checkout
                    .summary
                    .as_ref()
                    .filter(|summary| !summary.is_empty())
                    .map(|summary| {
                        format!(
                            "{} · +{} −{}",
                            summary.files, summary.added, summary.removed
                        )
                    });
                h_flex()
                    .id(("home-checkout", rank))
                    .w_full()
                    .gap_2()
                    .py_0p5()
                    .items_center()
                    .rounded(cx.theme().radius)
                    .cursor_pointer()
                    .hover(|el| el.bg(hovered))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_worktree(path.clone(), window, cx)
                    }))
                    .child(
                        icon(if checkout.here { "check" } else { "folder" })
                            .xsmall()
                            .text_color(if checkout.here { accent } else { muted }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .when(checkout.here, |el| el.font_semibold())
                            .child(checkout.label),
                    )
                    .when_some(checkout.branch, |el, branch| {
                        el.child(
                            div()
                                .flex_none()
                                .max_w(px(160.))
                                .truncate()
                                .text_xs()
                                .text_color(muted)
                                .child(branch),
                        )
                    })
                    .when_some(dirty, |el, dirty| {
                        el.child(div().flex_none().text_xs().text_color(muted).child(dirty))
                    })
                    // An agent is at work there: the one thing one scans this
                    // list for that no other panel says.
                    .when_some(checkout.agent, |el, working| {
                        el.child(icon("bot").xsmall().text_color(if working {
                            success
                        } else {
                            muted
                        }))
                    })
            }))
            .when(rest > 0, |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(tr!("home-more-checkouts", { count: rest })),
                )
            })
            .into_any_element()
    }
}

/// What the project brings with it: its recipes, its services, its server.
///
/// Each line is conditional, and the card goes when they all are: a project
/// with no `justfile`, no `wt.toml` and no language server has nothing to say
/// here, and an empty card would be one of the empty rooms.
fn render_home_project(
    recipes: Option<usize>,
    up: Option<bool>,
    lsp: bool,
    cx: &mut Context<ClaudhubApp>,
) -> AnyElement {
    if recipes.is_none() && up.is_none() && !lsp {
        return div().into_any_element();
    }
    let theme = cx.theme();
    let (muted, success) = (theme.muted_foreground, theme.success);
    card("home-project", "layout-dashboard", cx)
        .when_some(recipes, |el, count| {
            el.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(icon("zap").xsmall().text_color(muted))
                    .child(div().text_sm().child(tr!("home-recipes", { count: count }))),
            )
        })
        .when_some(up, |el, up| {
            el.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        icon(if up { "circle-check" } else { "circle-dashed" })
                            .xsmall()
                            .text_color(if up { success } else { muted }),
                    )
                    .child(div().text_sm().child(if up {
                        tr!("home-services-up")
                    } else {
                        tr!("home-services-down")
                    })),
            )
        })
        .when(lsp, |el| {
            el.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(icon("crosshair").xsmall().text_color(muted))
                    .child(div().text_sm().child(tr!("home-lsp"))),
            )
        })
        .into_any_element()
}

/// The four keys worth knowing before anything is open.
///
/// Read from the shortcuts table and not written out here: a key that is
/// customised in the settings must not be advertised as its default, and a help
/// that lies about the keyboard is worse than none. See `ui::shortcuts`.
fn render_home_keys(cx: &mut Context<ClaudhubApp>) -> AnyElement {
    const KEYS: [&str; 4] = [
        "shortcut-find-file",
        "shortcut-search-project",
        "shortcut-new-terminal",
        "shortcut-help",
    ];
    let overrides = &Settings::global(cx).shortcuts;
    let labels = crate::ui::shortcuts::Labels::current();
    let entries: Vec<(String, SharedString)> = KEYS
        .iter()
        .filter_map(|label| {
            let entry = crate::ui::shortcuts::all().find(|entry| &entry.label == label)?;
            let keys = entry.effective(overrides).trim();
            (!keys.is_empty()).then(|| {
                (
                    crate::ui::shortcuts::pretty(keys, &labels),
                    tr!(entry.label),
                )
            })
        })
        .collect();
    if entries.is_empty() {
        return div().into_any_element();
    }
    let theme = cx.theme();
    let (muted, border, secondary) = (theme.muted_foreground, theme.border, theme.secondary);
    let mono = theme.mono_font_family.clone();
    h_flex()
        .w_full()
        .gap_4()
        .flex_wrap()
        .items_center()
        .text_xs()
        .text_color(muted)
        .children(entries.into_iter().map(|(keys, label)| {
            h_flex()
                .gap_1p5()
                .items_center()
                .child(
                    div()
                        .px_1p5()
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(border)
                        .bg(secondary)
                        .font_family(mono.clone())
                        .child(keys),
                )
                .child(label)
        }))
        .into_any_element()
}

/// A section of the page: a heading, then whatever it has to say.
///
/// `flex_1` with a floor on the width: two of them share a row while the panel
/// is wide enough and wrap when it is not — the centre is a tab group one is
/// free to split down to a column.
fn card(title: &'static str, glyph: &'static str, cx: &App) -> gpui::Div {
    let theme = cx.theme();
    v_flex()
        .flex_1()
        .min_w(px(260.))
        .gap_1p5()
        .p_3()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.border)
        .bg(theme.secondary.opacity(0.4))
        .child(
            h_flex()
                .gap_1p5()
                .items_center()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(icon(glyph).xsmall())
                .child(tr!(title)),
        )
}

/// A figure with its glyph: `↑2`, `+410`.
fn chip(glyph: &'static str, text: SharedString, tone: gpui::Hsla) -> gpui::Div {
    h_flex()
        .gap_0p5()
        .items_center()
        .text_xs()
        .text_color(tone)
        .child(icon(glyph).xsmall())
        .child(text)
}

/// A word one clicks, painted as one: the page's only gesture besides the rows.
fn link(
    id: &'static str,
    label: SharedString,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> gpui::Stateful<gpui::Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .mt_1()
        .text_xs()
        .text_color(theme.accent_foreground)
        .cursor_pointer()
        .hover(|el| el.text_color(theme.foreground))
        .on_click(on_click)
        .child(label)
}

/// The page with nothing to show: no repository is open yet.
fn home_note(text: SharedString, cx: &App) -> AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .child(
            icon("house")
                .large()
                .text_color(cx.theme().muted_foreground.opacity(0.4)),
        )
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(text),
        )
        .into_any_element()
}
