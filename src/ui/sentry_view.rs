//! The Sentry panel: issues, their trace, and what is needed to hand them over.
//!
//! This is the complete loop this milestone closes: from the error report to
//! the reviewed worktree. You read an issue, click a frame to open the
//! offending code, and hand the whole thing to an agent — either in the current
//! worktree, or in a fresh worktree created for the occasion.
//!
//! Claudhub **sends nothing** to Sentry: it reads.

use std::path::PathBuf;

use gpui::{div, prelude::*, px, Context, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex, v_flex, ActiveTheme, Disableable, Sizable,
};

use crate::runtime::{Cmd, Secret};
use crate::sentry::{Event, Issue};
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::settings::Settings;
use crate::ui::store::Store;

/// What the Sentry panel shows.
#[derive(Default)]
pub struct SentryState {
    pub issues: Vec<Issue>,
    pub selected: Option<String>,
    /// The chosen issue's event, once it has arrived.
    pub event: Option<Event>,
    /// A request has gone out and not come back. Without this guard, every frame
    /// of the panel would call the API again.
    pub loading: bool,
    /// A request has already been made: the panel does not reload by itself, a
    /// remote API having no business being queried on every tab opening.
    pub asked: bool,
}

impl ClaudhubApp {
    /// The current repository's Sentry project, as the store remembers it.
    ///
    /// The project belongs to the **repository** and not to the account: two
    /// repositories of the same organisation do not have the same errors.
    fn sentry_project(&self, cx: &gpui::App) -> Option<String> {
        let main = self.main_of(self.active.as_deref()?)?;
        Store::global(cx)
            .repos
            .get(&main)
            .and_then(|repo| repo.sentry_project.clone())
            .filter(|project| !project.trim().is_empty())
    }

    pub(super) fn set_sentry_project(&mut self, project: String, cx: &mut Context<Self>) {
        let Some(main) = self.active.as_deref().and_then(|w| self.main_of(w)) else {
            return;
        };
        let project = project.trim().to_string();
        Store::update_global(cx, |store| {
            store.repos.entry(main.clone()).or_default().sentry_project =
                (!project.is_empty()).then_some(project.clone());
        });
        self.sentry.asked = false;
        self.load_issues(cx);
    }

    pub(super) fn load_issues(&mut self, cx: &mut Context<Self>) {
        let settings = Settings::global(cx);
        let (org, query) = (
            settings.sentry_org.trim().to_string(),
            settings.sentry_query.clone(),
        );
        let Some(project) = self.sentry_project(cx) else {
            return;
        };
        if org.is_empty() || self.sentry.loading {
            return;
        }
        self.sentry.loading = true;
        self.sentry.asked = true;
        let token = Secret(Settings::global(cx).sentry_token.clone());
        self.git.send(Cmd::LoadIssues {
            org,
            project,
            query,
            token,
        });
        cx.notify();
    }

    pub(super) fn issues_arrived(&mut self, issues: Vec<Issue>, cx: &mut Context<Self>) {
        self.sentry.loading = false;
        self.sentry.issues = issues;
        cx.notify();
    }

    pub(super) fn issue_event_arrived(
        &mut self,
        issue: String,
        event: Event,
        cx: &mut Context<Self>,
    ) {
        // A late answer, for an issue no longer being looked at, would replace
        // the trace with the wrong one.
        if self.sentry.selected.as_deref() == Some(issue.as_str()) {
            self.sentry.event = Some(event);
            cx.notify();
        }
    }

    pub(super) fn select_issue(&mut self, id: String, cx: &mut Context<Self>) {
        if self.sentry.selected.as_deref() == Some(id.as_str()) {
            return;
        }
        self.sentry.selected = Some(id.clone());
        self.sentry.event = None;
        let token = Secret(Settings::global(cx).sentry_token.clone());
        self.git.send(Cmd::LoadIssueEvent { issue: id, token });
        cx.notify();
    }

    /// An issue's prompt, once its trace is here.
    fn issue_prompt(&self) -> Option<String> {
        let worktree = self.active.as_deref()?;
        let event = self.sentry.event.as_ref()?;
        let id = self.sentry.selected.as_deref()?;
        let issue = self.sentry.issues.iter().find(|issue| issue.id == id)?;
        let intro = crate::tr!("sentry-prompt-intro", { title: issue.title.clone() });
        Some(crate::sentry::prompt(&intro, issue, event, worktree))
    }

    /// Hands the issue to the current worktree's agent.
    pub(super) fn hand_issue_to_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(text), Some(worktree)) = (self.issue_prompt(), self.active.clone()) else {
            return;
        };
        self.show_terminal_panel(window, cx);
        self.send_to_agent(&worktree, text, window, cx);
    }

    /// Opens a worktree for this issue, and starts the agent there with the
    /// report.
    ///
    /// This is the complete loop: from the error report to the reviewed
    /// worktree. Creation goes through `wt`, so through the project's copies,
    /// ports and hooks — a worktree where the agent can start work at once.
    pub(super) fn open_worktree_for_issue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(text), Some(id)) = (self.issue_prompt(), self.sentry.selected.clone()) else {
            return;
        };
        let Some(main) = self.active.as_deref().and_then(|w| self.main_of(w)) else {
            return;
        };
        let slug = format!("sentry-{id}");
        // The worktree does not exist yet: the prompt is held and delivered when
        // the worktree list arrives, once `wt` has finished its hooks.
        if let Some(root) = self.wt_project(&main).map(|project| project.root.clone()) {
            self.awaiting_agent = Some((root.join(&slug), text));
        }
        self.start_worktree(main, slug, None, window, cx);
    }

    /// Delivers the pending prompt, if the worktree it targets has just arrived.
    ///
    /// Called on every `Evt::Worktrees`: it is the only signal that says `wt`
    /// has finished — creation runs hooks that take minutes, and nothing else
    /// marks their end.
    pub(super) fn deliver_awaited_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((path, _)) = self.awaiting_agent.as_ref() else {
            return;
        };
        let path = path.clone();
        if !self
            .repos
            .iter()
            .flat_map(|repo| repo.worktrees.iter())
            .any(|w| w.path == path)
        {
            return;
        }
        let Some((_, text)) = self.awaiting_agent.take() else {
            return;
        };
        self.select_worktree(path.clone(), window, cx);
        self.show_terminal_panel(window, cx);
        self.send_to_agent(&path, text, window, cx);
    }

    /// Opens a frame's file, at its line.
    ///
    /// The external editor first, if it is configured: it is the one that knows
    /// how to open at a line. Failing that, the built-in editor, which at least
    /// opens the right file.
    fn open_frame(&mut self, path: PathBuf, line: usize, cx: &mut Context<Self>) {
        if Settings::global(cx).external_editor.trim().is_empty() {
            self.open_in_editor(path, cx);
        } else {
            self.open_externally(path, line, cx);
        }
    }

    pub(super) fn render_sentry(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let issues_scroll = self.scroll_of("sentry-issues");
        let find = self.render_find(crate::ui::find::Pane::Sentry, cx);
        let query = self.query(crate::ui::find::Pane::Sentry, cx);
        let org = Settings::global(cx).sentry_org.trim().to_string();
        let project = self.sentry_project(cx);
        let muted = cx.theme().muted_foreground;

        let bar =
            h_flex()
                .h(crate::ui::theme::bar_height(cx))
                .w_full()
                .px_2()
                .gap_2()
                .items_center()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(icon("triangle-alert").xsmall())
                .child(div().flex_1().truncate().text_xs().text_color(muted).child(
                    SharedString::from(match &project {
                        Some(project) => format!("{org}/{project}"),
                        None => tr!("sentry-no-project").to_string(),
                    }),
                ))
                .child(
                    Button::new("sentry-project")
                        .ghost()
                        .xsmall()
                        .icon(icon("settings"))
                        .tooltip(tr!("sentry-set-project"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.prompt_sentry_project(window, cx)
                        })),
                )
                .child(
                    Button::new("sentry-reload")
                        .ghost()
                        .xsmall()
                        .icon(icon("refresh-cw"))
                        .tooltip(tr!("action-refresh"))
                        .disabled(self.sentry.loading)
                        .on_click(cx.listener(|this, _, _window, cx| {
                            this.sentry.loading = false;
                            this.load_issues(cx);
                        })),
                );

        if org.is_empty() || project.is_none() {
            return v_flex()
                .size_full()
                .child(bar)
                .child(empty(tr!("sentry-configure"), cx))
                .into_any_element();
        }
        // The first load happens at render time, once only: a remote API has no
        // business being queried on every tab opening, nor on every frame.
        if !self.sentry.asked {
            self.load_issues(cx);
        }

        // The filter applies to what is already loaded, and not to the query
        // sent to Sentry (`sentry_query`, a setting): you search among the
        // issues in front of you, without waiting for a network round trip.
        let issues: Vec<_> = self
            .sentry
            .issues
            .iter()
            .filter(|issue| {
                crate::ui::find::matches(&query, &issue.title)
                    || crate::ui::find::matches(&query, &issue.culprit)
            })
            .cloned()
            .collect();
        let selected = self.sentry.selected.clone();
        if issues.is_empty() {
            return v_flex()
                .size_full()
                .child(bar)
                .children(find)
                .child(empty(
                    if self.sentry.loading {
                        tr!("sentry-loading")
                    } else {
                        tr!("sentry-empty")
                    },
                    cx,
                ))
                .into_any_element();
        }

        let mut rows = Vec::new();
        for (index, issue) in issues.into_iter().enumerate() {
            rows.push(self.render_issue(index, issue, selected.as_deref(), cx));
        }

        v_flex()
            .size_full()
            .child(bar)
            .children(find)
            .child(
                div().flex_1().min_h_0().child(
                    self.scrolled(
                        "sentry-issues-bar",
                        &issues_scroll,
                        crate::ui::motion::Axes::Vertical,
                        window,
                        v_flex()
                            .id("sentry-issues")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&issues_scroll)
                            .children(rows),
                        cx,
                    ),
                ),
            )
            .children(self.render_trace(window, cx))
            .into_any_element()
    }

    fn render_issue(
        &mut self,
        index: usize,
        issue: Issue,
        selected: Option<&str>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_selected = selected == Some(issue.id.as_str());
        let muted = cx.theme().muted_foreground;
        let color = match issue.level.as_str() {
            "fatal" | "error" => cx.theme().danger,
            "warning" => cx.theme().warning,
            _ => muted,
        };
        let id = issue.id.clone();
        v_flex()
            .id(("issue", index))
            .px_2()
            .py_1()
            .gap_1()
            .cursor_pointer()
            .border_b_1()
            .border_color(cx.theme().border)
            .when(is_selected, |el| el.bg(cx.theme().accent))
            .hover(|s| s.bg(cx.theme().accent.opacity(0.4)))
            .on_click(cx.listener(move |this, _, _window, cx| {
                this.select_issue(id.clone(), cx);
            }))
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(div().size(px(6.)).rounded_full().bg(color))
                    .child(div().flex_1().truncate().text_sm().child(issue.title))
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(issue.count.to_string()),
                    ),
            )
            .when(!issue.culprit.is_empty(), |el| {
                el.child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(muted)
                        .child(issue.culprit),
                )
            })
    }

    /// The chosen issue's trace, with its clickable frames.
    fn render_trace(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let trace_scroll = self.scroll_of("sentry-trace");
        let worktree = self.active.clone()?;
        let event = self.sentry.event.clone()?;
        let (muted, mono) = (
            cx.theme().muted_foreground,
            cx.theme().mono_font_family.clone(),
        );
        let has_prompt = self.issue_prompt().is_some();

        let mut frames = Vec::new();
        for (index, frame) in event.frames.iter().enumerate() {
            let path = frame.repo_path(&worktree);
            let line = frame.line;
            let target = PathBuf::from(&path);
            let label = SharedString::from(if frame.function.is_empty() {
                format!("{path}:{line}")
            } else {
                format!("{path}:{line} · {}", frame.function)
            });
            frames.push(
                div()
                    .id(("frame", index))
                    .px_2()
                    .py_0p5()
                    .truncate()
                    .text_xs()
                    .font_family(mono.clone())
                    .cursor_pointer()
                    // The application's frames stand out: that is where the bug
                    // is, the rest is the path that led there.
                    .when(!frame.in_app, |el| el.text_color(muted))
                    .hover(|s| s.bg(cx.theme().accent.opacity(0.4)))
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.open_frame(target.clone(), line.max(1), cx);
                    }))
                    .child(label)
                    .into_any_element(),
            );
        }

        Some(
            v_flex()
                .max_h(px(260.))
                .border_t_1()
                .border_color(cx.theme().border)
                .child(
                    h_flex()
                        .px_2()
                        .py_1()
                        .gap_1()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .truncate()
                                .text_xs()
                                .text_color(muted)
                                .child(SharedString::from(event.message.clone())),
                        )
                        .child(
                            Button::new("sentry-hand")
                                .outline()
                                .xsmall()
                                .label(tr!("sentry-hand-to-agent"))
                                .disabled(!has_prompt)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.hand_issue_to_agent(window, cx)
                                })),
                        )
                        .child(
                            Button::new("sentry-worktree")
                                .ghost()
                                .xsmall()
                                .label(tr!("sentry-open-worktree"))
                                .disabled(!has_prompt)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_worktree_for_issue(window, cx)
                                })),
                        ),
                )
                .child(
                    div().flex_1().min_h_0().child(
                        self.scrolled(
                            "sentry-trace-bar",
                            &trace_scroll,
                            crate::ui::motion::Axes::Vertical,
                            window,
                            v_flex()
                                .id("sentry-trace")
                                .size_full()
                                .overflow_y_scroll()
                                .track_scroll(&trace_scroll)
                                .children(frames),
                            cx,
                        ),
                    ),
                ),
        )
    }

    fn prompt_sentry_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.sentry_project(cx).unwrap_or_default();
        self.open_text_dialog(
            tr!("sentry-set-project"),
            SharedString::from(current),
            window,
            cx,
            |this, value, _window, cx| this.set_sentry_project(value, cx),
        );
    }
}

fn empty(message: SharedString, cx: &Context<ClaudhubApp>) -> impl IntoElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .text_color(cx.theme().muted_foreground)
        .child(icon("triangle-alert"))
        .child(div().text_sm().px_4().child(message))
}
