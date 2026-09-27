//! A board's PR tab: the branch's pull request, or the form that opens one.
//!
//! **Without a pull request, the form is the tab** — no dialog: the title and
//! the body proposed from the branch's commits (`github::draft_from`), the base
//! the review compares against, and whether it opens as a draft. The branch is
//! pushed first when it was never published, and the tab says so.
//!
//! **With one**, what a reviewer looks at: its state and where it goes, the
//! gestures that move it — ready for review or back to a draft, merge —, its
//! checks and last CI run, and its review threads, unresolved first, each
//! answered or resolved from here (`github::reply_command`,
//! `github::resolve_command`). Every gesture reads the pull request again once
//! GitHub has answered.
//!
//! It speaks of the worktree on show, as the GitHub panel does: the state it
//! reads (`GithubState`) follows one worktree.

use std::path::{Path, PathBuf};

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputState, Textarea, TextareaState},
    menu::{DropdownMenu as _, PopupMenuItem},
    v_flex, ActiveTheme, Disableable as _, Sizable as _,
};
use gpui_kit::{div, prelude::*, px, AnyElement, Context, Entity, SharedString, Window};

use crate::github::{
    merge_command, ready_command, reply_command, resolve_command, threads_command, MergeHow, Stage,
    Thread,
};
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::github::{base_for_gh, commits_command, draft_from, PullRequest};
use crate::ui::icons::icon;

/// The form that opens a pull request, for one worktree.
pub(super) struct PrForm {
    worktree: PathBuf,
    title: Entity<InputState>,
    body: Entity<TextareaState>,
    base: Entity<InputState>,
    draft: bool,
}

impl ClaudhubApp {
    /// The PR tab of the worktree at `path`.
    pub(super) fn render_pr_view(
        &mut self,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.active.as_deref() != Some(path) {
            return self.follow_the_active(path, cx);
        }
        self.ensure_github(cx);
        // The one reply field, made here where a window is at hand: a field
        // recreated per frame would lose its text at the first keystroke.
        if self.pr_reply.is_none() {
            self.pr_reply =
                Some(cx.new(|cx| {
                    InputState::new(window, cx).placeholder(tr!("pr-thread-placeholder"))
                }));
        }
        let body = match self.branch_pr().cloned() {
            Some(pr) => {
                self.ensure_pr_threads(pr.number);
                self.render_pr_page(&pr, cx)
            }
            None if self.github.pr_loading => note(tr!("github-pr-loading"), cx),
            None => self.render_pr_form(path, window, cx),
        };
        v_flex()
            .id("focus-pr")
            .size_full()
            .gap_3()
            .overflow_y_scroll()
            .children(
                self.github
                    .error
                    .clone()
                    .map(|error| div().text_xs().text_color(cx.theme().danger).child(error)),
            )
            .child(body)
            .into_any_element()
    }

    /// Asks for a pull request's review threads, once per pull request and
    /// per reading.
    pub(super) fn ensure_pr_threads(&mut self, number: u64) {
        let Some(active) = self.active.clone() else {
            return;
        };
        if self.github.view_threads_for == Some(number) || self.github.view_threads_loading {
            return;
        }
        self.github.view_threads_for = Some(number);
        self.github.view_threads_loading = true;
        self.github.view_threads_call = self.ask_gh(threads_command(number), active);
    }

    /// A gesture on the pull request, sent through `gh` — see
    /// `GithubState::pr_action_call`.
    fn pr_action(&mut self, command: String, cx: &mut Context<Self>) {
        let Some(active) = self.active.clone() else {
            return;
        };
        self.github.pr_action_call = self.ask_gh(command, active);
        cx.notify();
    }

    // — The form ——————————————————————————————————————————————————

    /// The form, made the first time the tab shows it for this worktree,
    /// and filled from the branch's commits once they are read.
    fn render_pr_form(
        &mut self,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self
            .pr_form
            .as_ref()
            .is_none_or(|form| form.worktree != path)
        {
            let base = self.base_here().map(|base| base_for_gh(&base).to_string());
            let branch = self.branch_here().unwrap_or_default();
            let form = PrForm {
                worktree: path.to_path_buf(),
                title: cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(tr!("github-new-title-placeholder"))
                        .default_value(branch)
                }),
                body: cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .auto_grow(6, 20)
                        .placeholder(tr!("github-new-body-placeholder"))
                }),
                base: cx.new(|cx| {
                    InputState::new(window, cx).default_value(base.clone().unwrap_or_default())
                }),
                draft: false,
            };
            self.pr_form = Some(form);
            if let Some(base) = self.base_here() {
                self.github.form_call = self.ask_gh(commits_command(&base), path.to_path_buf());
            }
        }
        let Some(form) = self.pr_form.as_ref() else {
            return div().into_any_element();
        };
        let theme = cx.theme().clone();
        let label = |text: SharedString| {
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(text)
        };
        let (title, body, base, draft) = (
            form.title.clone(),
            form.body.clone(),
            form.base.clone(),
            form.draft,
        );
        let creating = self.github.creating;
        let push = self.unpublished();
        v_flex()
            .w_full()
            .max_w(px(820.))
            .p_4()
            .gap_3()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(icon("git-pull-request").text_color(theme.muted_foreground))
                    .child(label(tr!("github-new-title"))),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(label(tr!("pr-form-title")))
                    .child(Input::new(&title).small()),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(label(tr!("pr-form-body")))
                    .child(Textarea::new(&body)),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(label(tr!("pr-form-base")))
                    .child(div().w(px(220.)).child(Input::new(&base).small()))
                    .child(
                        Checkbox::new("pr-form-draft")
                            .label(tr!("pr-form-draft"))
                            .checked(draft)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(form) = this.pr_form.as_mut() {
                                    form.draft = !form.draft;
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .when(push, |el| el.child(tr!("pr-form-push"))),
                    )
                    .child(
                        Button::new("pr-form-create")
                            .primary()
                            .icon(icon("git-pull-request"))
                            .label(tr!("pr-form-create"))
                            .loading(creating)
                            .disabled(creating)
                            .on_click(cx.listener(|this, _, _, cx| this.submit_pr_form(cx))),
                    ),
            )
            .into_any_element()
    }

    /// The branch's commits are read: the form proposes their title and body,
    /// unless the hand has written in it meanwhile.
    pub(super) fn fill_pr_form(
        &mut self,
        subjects: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let branch = self.branch_here().unwrap_or_default();
        let Some(form) = self.pr_form.as_ref() else {
            return;
        };
        let (title, body) = draft_from(subjects, &branch);
        let untouched = form.title.read(cx).value() == branch.as_str();
        if untouched {
            form.title
                .update(cx, |state, cx| state.set_value(title, window, cx));
        }
        if form.body.read(cx).value().is_empty() {
            form.body
                .update(cx, |state, cx| state.set_value(body, window, cx));
        }
        cx.notify();
    }

    fn submit_pr_form(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.pr_form.as_ref() else {
            return;
        };
        let title = form.title.read(cx).value().to_string();
        let body = form.body.read(cx).value().to_string();
        let base = form.base.read(cx).value().trim().to_string();
        let draft = form.draft;
        self.create_pr((!base.is_empty()).then_some(base), title, body, draft, cx);
    }

    // — The pull request ———————————————————————————————————————————

    /// The pull request: its head, its checks, its threads.
    fn render_pr_page(&mut self, pr: &PullRequest, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .w_full()
            .gap_3()
            .child(self.render_pr_head(pr, cx))
            .child(self.render_pr_checks_card(pr, cx))
            .child(self.render_pr_threads(cx))
            .into_any_element()
    }

    /// What the pull request is, where it goes, and what moves it.
    fn render_pr_head(&self, pr: &PullRequest, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let (state, tint) = if pr.is_draft {
            (tr!("pr-state-draft"), muted)
        } else {
            (tr!("pr-state-open"), theme.success)
        };
        let url = pr.url.clone();
        let number = pr.number;
        let is_draft = pr.is_draft;
        let (merge_number, merge_base) = (pr.number, pr.base_ref_name.clone());
        let app = cx.entity().downgrade();
        let merge_menu = Button::new("pr-merge")
            .small()
            .icon(icon("git-merge"))
            .label(tr!("pr-merge"))
            .disabled(is_draft || pr.conflicts())
            .dropdown_menu(move |menu, _, _| {
                [
                    (MergeHow::Merge, tr!("pr-merge-merge")),
                    (MergeHow::Squash, tr!("pr-merge-squash")),
                    (MergeHow::Rebase, tr!("pr-merge-rebase")),
                ]
                .into_iter()
                .fold(menu, |menu, (how, label)| {
                    let (app, base) = (app.clone(), merge_base.clone());
                    menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                        if let Some(app) = app.upgrade() {
                            let base = base.clone();
                            app.update(cx, |this, cx| {
                                this.confirm_pr_merge(merge_number, base, how, window, cx)
                            });
                        }
                    }))
                })
            });
        v_flex()
            .w_full()
            .p_3()
            .gap_2()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(icon("git-pull-request").text_color(tint))
                    .child(
                        div()
                            .text_color(muted)
                            .child(SharedString::from(format!("#{}", pr.number))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(SharedString::from(pr.title.clone())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .px_2()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(tint)
                            .text_xs()
                            .text_color(tint)
                            .child(state),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .text_color(muted)
                    .child(SharedString::from(format!(
                        "{} → {}",
                        pr.head_ref_name, pr.base_ref_name
                    )))
                    .children(
                        pr.review_note()
                            .map(|note| div().child(format!("· {note}"))),
                    )
                    .when(pr.conflicts(), |el| {
                        el.child(div().text_color(theme.danger).child(tr!("pr-conflicts")))
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("pr-open")
                            .small()
                            .ghost()
                            .icon(icon("external-link"))
                            .label(tr!("pr-open"))
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                    .child(
                        Button::new("pr-ready")
                            .small()
                            .ghost()
                            .icon(icon(if is_draft { "eye" } else { "pencil" }))
                            .label(if is_draft {
                                tr!("pr-ready")
                            } else {
                                tr!("pr-undraft")
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.pr_action(ready_command(number, is_draft), cx);
                            })),
                    )
                    .child(merge_menu),
            )
            .into_any_element()
    }

    /// Asks before merging: it cannot be taken back from here.
    fn confirm_pr_merge(
        &mut self,
        number: u64,
        base: String,
        how: MergeHow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let way = match how {
            MergeHow::Merge => tr!("pr-merge-merge"),
            MergeHow::Squash => tr!("pr-merge-squash"),
            MergeHow::Rebase => tr!("pr-merge-rebase"),
        };
        let body = tr!("pr-merge-body", { n: number, base: base, way: way });
        crate::ui::dialogs::ask(
            cx.entity(),
            tr!("pr-merge-title"),
            move || div().text_sm().child(body.clone()).into_any_element(),
            || crate::ui::dialogs::submit(tr!("pr-merge")),
            move |this, _, cx| this.pr_action(merge_command(number, how), cx),
            window,
            cx,
        );
    }

    /// The checks, each with its state — a press opens it —, and the
    /// branch's last CI run.
    fn render_pr_checks_card(&self, pr: &PullRequest, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let tint = |stage: Stage| match stage {
            Stage::Passed => theme.success,
            Stage::Failed => theme.danger,
            Stage::Running | Stage::Waiting => theme.warning,
            Stage::Skipped => muted,
        };
        let checks = pr.checks();
        let rows: Vec<AnyElement> = pr
            .status_check_rollup
            .iter()
            .enumerate()
            .map(|(index, check)| {
                let link = check.link().to_string();
                let stage = check.stage();
                h_flex()
                    .id(("pr-check", index))
                    .gap_1p5()
                    .items_center()
                    .text_sm()
                    .when(!link.is_empty(), |el| {
                        el.cursor_pointer()
                            .on_click(move |_, _, cx| cx.open_url(&link))
                    })
                    .child(icon(stage.glyph()).xsmall().text_color(tint(stage)))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(check.label().to_string())),
                    )
                    .into_any_element()
            })
            .collect();
        let run = self.github.runs.first().cloned().map(|run| {
            let url = run.url.clone();
            let stage = run.stage();
            h_flex()
                .id("pr-run")
                .gap_1p5()
                .items_center()
                .text_xs()
                .cursor_pointer()
                .on_click(move |_, _, cx| cx.open_url(&url))
                .child(icon(run.glyph()).xsmall().text_color(tint(stage)))
                .child(
                    div()
                        .text_color(muted)
                        .child(SharedString::from(run.workflow.clone())),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(run.title.clone())),
                )
        });
        v_flex()
            .w_full()
            .p_3()
            .gap_1p5()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(tr!("pr-checks")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(tr!("pr-checks-tally", {
                                passed: checks.passed,
                                failed: checks.failed,
                                running: checks.running
                            })),
                    ),
            )
            .when(rows.is_empty(), |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(tr!("pr-checks-none")),
                )
            })
            .children(rows)
            .children(run)
            .into_any_element()
    }

    /// The review threads: the unresolved ones, then — unfolded on demand —
    /// the resolved; each with its place, its comments, and its gestures.
    fn render_pr_threads(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let threads = self.github.view_threads.clone();
        let open: Vec<&Thread> = crate::github::unresolved(&threads);
        let resolved: Vec<&Thread> = threads.iter().filter(|thread| thread.resolved).collect();
        let mut rows: Vec<AnyElement> = Vec::new();
        for thread in &open {
            rows.push(self.render_thread(thread, cx));
        }
        if self.github.resolved_open {
            for thread in &resolved {
                rows.push(self.render_thread(thread, cx));
            }
        }
        let loading = self.github.view_threads_loading;
        let shown = self.github.resolved_open;
        v_flex()
            .w_full()
            .p_3()
            .gap_2()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(tr!("pr-threads")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(if open.is_empty() {
                                muted
                            } else {
                                theme.warning
                            })
                            .child(tr!("pr-threads-open", { count: open.len() })),
                    )
                    .child(div().flex_1())
                    .when(!resolved.is_empty(), |el| {
                        el.child(
                            Button::new("pr-threads-resolved")
                                .ghost()
                                .xsmall()
                                .label(if shown {
                                    tr!("pr-threads-hide-resolved")
                                } else {
                                    tr!("pr-threads-show-resolved", { count: resolved.len() })
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.github.resolved_open = !this.github.resolved_open;
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .when(loading && threads.is_empty(), |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(tr!("github-pr-loading")),
                )
            })
            .when(!loading && threads.is_empty(), |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(tr!("pr-threads-none")),
                )
            })
            .children(rows)
            .into_any_element()
    }

    /// One thread: where it is — outdated, resolved —, who said what, and
    /// the reply and resolution gestures, the reply field under it when it is
    /// the one being answered.
    fn render_thread(&mut self, thread: &Thread, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let place = match thread.line.or(thread.original_line) {
            Some(line) => format!("{}:{line}", thread.path),
            None => thread.path.clone(),
        };
        let id = thread.id.clone();
        let replying = self.github.replying.as_deref() == Some(id.as_str());
        let (reply_id, resolve_id, send_id) = (id.clone(), id.clone(), id.clone());
        let resolved = thread.resolved;
        let reply_field = self.pr_reply.clone().filter(|_| replying);
        v_flex()
            .w_full()
            .p_2()
            .gap_1p5()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .when(resolved, |el| el.opacity(0.7))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(icon("file-text").xsmall().text_color(muted))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .child(SharedString::from(place)),
                    )
                    .when(thread.outdated, |el| {
                        el.child(
                            div()
                                .text_xs()
                                .text_color(theme.warning)
                                .child(tr!("pr-thread-outdated")),
                        )
                    })
                    .when(resolved, |el| {
                        el.child(
                            div()
                                .text_xs()
                                .text_color(theme.success)
                                .child(tr!("pr-thread-resolved")),
                        )
                    })
                    .child(div().flex_1())
                    .when(!id.is_empty(), |el| {
                        el.child(
                            Button::new(SharedString::from(format!("pr-reply-{id}")))
                                .ghost()
                                .xsmall()
                                .icon(icon("message-square-plus"))
                                .label(tr!("pr-thread-reply"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.github.replying = Some(reply_id.clone());
                                    if let Some(input) = this.pr_reply.clone() {
                                        input.update(cx, |state, cx| {
                                            state.set_value("", window, cx)
                                        });
                                        crate::ui::dialogs::focus_field(&input, window, cx);
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new(SharedString::from(format!("pr-resolve-{id}")))
                                .ghost()
                                .xsmall()
                                .icon(icon(if resolved { "undo-2" } else { "check" }))
                                .label(if resolved {
                                    tr!("pr-thread-reopen")
                                } else {
                                    tr!("pr-thread-resolve")
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.pr_action(resolve_command(&resolve_id, !resolved), cx);
                                })),
                        )
                    }),
            )
            .children(thread.comments.iter().map(|comment| {
                v_flex()
                    .gap_0p5()
                    .pl_4()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(SharedString::from(format!("@{}", comment.author))),
                    )
                    .child(
                        div()
                            .text_sm()
                            .child(SharedString::from(comment.body.clone())),
                    )
            }))
            .children(reply_field.map(|field| {
                h_flex()
                    .pl_4()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().child(Input::new(&field).small()))
                    .child(
                        Button::new("pr-reply-send")
                            .small()
                            .primary()
                            .label(tr!("pr-thread-send"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let Some(body) = this
                                    .pr_reply
                                    .as_ref()
                                    .map(|input| input.read(cx).value().trim().to_string())
                                    .filter(|body| !body.is_empty())
                                else {
                                    return;
                                };
                                this.pr_action(reply_command(&send_id, &body), cx);
                            })),
                    )
                    .child(
                        Button::new("pr-reply-cancel")
                            .small()
                            .ghost()
                            .label(tr!("dialog-cancel"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.github.replying = None;
                                cx.notify();
                            })),
                    )
            }))
            .into_any_element()
    }
}

/// A line of text where a card has nothing else to say.
fn note(text: SharedString, cx: &mut Context<ClaudhubApp>) -> AnyElement {
    div()
        .p_3()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text)
        .into_any_element()
}
