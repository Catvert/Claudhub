//! The CI runs of the branch one is on, read through `gh`.
//!
//! **`gh` and not GitHub's API**, which is the whole reason this view is cheap:
//! the CLI is already authenticated — it is a program the user installs, like
//! the agents in the terminal — and Claudhub has no token to hold, no OAuth
//! flow to walk and no host to ask about. What it costs is a process per read,
//! on the background queue, which is the `wt` sweep's profile.
//!
//! The formatting is asked of `gh --template` rather than done here for the
//! same reason it was when this was a plugin: the JSON one would parse is one
//! more shape to keep up with, and a tab-separated line is a format that cannot
//! drift. One trap is written into the template and it is not a refinement —
//! `printf "%.0f"` on the id, because a Go template formats a JSON number as a
//! float, so `{{.databaseId}}` gives `3.2494024323e+10`, an id `gh run view`
//! does not recognise, and nothing says so before the first click.

use std::path::PathBuf;

use gpui::{div, prelude::*, Context, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex, v_flex, ActiveTheme, Disableable as _, Sizable as _,
};

use crate::runtime::protocol::Caller;
use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::find::Pane;
use crate::ui::icons::icon;

/// How many runs the list asks for. Twenty is a branch's recent history: past
/// that one is reading the project's, which is what the web page is for.
const LIMIT: usize = 20;

/// How many lines of a log are kept on screen. The **end**, which is where the
/// failure is written.
const TAIL: usize = 80;

/// How many go to the agent — more than one reads, because the agent reads it
/// all and the line that explains is not always the last.
const TAIL_FOR_AGENT: usize = 120;

/// One run of GitHub Actions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Run {
    pub id: String,
    pub title: String,
    pub workflow: String,
    /// `completed`, `in_progress`…
    pub status: String,
    /// `success`, `failure`, empty while it runs.
    pub conclusion: String,
}

impl Run {
    /// What the row says on its right: the conclusion once there is one, the
    /// status until then.
    pub fn tally(&self) -> &str {
        if self.conclusion.is_empty() {
            &self.status
        } else {
            &self.conclusion
        }
    }

    /// The glyph. A run in flight has its own, which is what tells "not yet"
    /// from "not well".
    pub fn glyph(&self) -> &'static str {
        match self.conclusion.as_str() {
            "success" => "circle-check",
            "" => "loader-circle",
            _ => "circle-x",
        }
    }

    /// Whether the filter's word is in what the row shows.
    pub fn matches(&self, needle: &str) -> bool {
        let needle = needle.trim().to_lowercase();
        needle.is_empty()
            || self.title.to_lowercase().contains(&needle)
            || self.workflow.to_lowercase().contains(&needle)
    }
}

/// The command that lists them. Written here rather than in the settings: it is
/// a format this module parses, so the two cannot be allowed to drift apart.
pub fn list_command() -> String {
    format!(
        "gh run list --limit {LIMIT} \
         --json databaseId,displayTitle,workflowName,status,conclusion \
         --template '{{{{range .}}}}{{{{printf \"%.0f\" .databaseId}}}}{{{{\"\\t\"}}}}\
         {{{{.displayTitle}}}}{{{{\"\\t\"}}}}{{{{.workflowName}}}}{{{{\"\\t\"}}}}\
         {{{{.status}}}}{{{{\"\\t\"}}}}{{{{.conclusion}}}}{{{{\"\\n\"}}}}{{{{end}}}}'"
    )
}

/// The command that reads one run's failing log.
pub fn log_command(id: &str) -> String {
    format!("gh run view {id} --log-failed")
}

/// One line per run, five fields apart by a tab — it is `gh` that formats.
///
/// A short line is skipped rather than failing the read: `gh` writes its own
/// notices to stdout on occasion, and one of them must not empty the list.
pub fn parse(out: &str) -> Vec<Run> {
    out.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() < 5 {
                return None;
            }
            Some(Run {
                id: fields[0].to_string(),
                title: fields[1].to_string(),
                workflow: fields[2].to_string(),
                status: fields[3].to_string(),
                conclusion: fields[4].to_string(),
            })
        })
        .collect()
}

/// The end of a log, which is the part the failure is written in.
pub fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    all[start..].join("\n")
}

/// What the view shows and what it is waiting for.
#[derive(Default)]
pub struct CiState {
    /// The worktree this reading is about, which is what says it is stale.
    pub worktree: Option<PathBuf>,
    pub runs: Vec<Run>,
    pub chosen: Option<usize>,
    /// The log of the chosen run, once asked for: choosing a row calls nothing,
    /// the button is what asks.
    pub log: Option<SharedString>,
    /// The worktree this reading was made for. `None` is "never read".
    ///
    /// The same device as `SentryState::read_for`, and simpler for the same
    /// job: what a run list depends on is one checkout and nothing else.
    pub read_for: Option<PathBuf>,
    pub loading: bool,
    pub error: Option<SharedString>,
    /// The sends in flight — a late answer is dropped rather than shown.
    pub list_call: u64,
    pub log_call: u64,
    /// What the log, once it lands, is for: reading, or handing over.
    pub log_for_agent: bool,
    pub scroll: gpui::UniformListScrollHandle,
}

impl ClaudhubApp {
    fn ask_gh(&mut self, command: String, worktree: PathBuf) -> u64 {
        self.ci_seq += 1;
        let call = self.ci_seq;
        self.git.send(Cmd::Call {
            caller: Caller::Ci,
            call,
            cap: crate::outside::Cap::Shell { worktree, command },
        });
        call
    }

    /// Reads the runs **the first time the panel is drawn**, and never before.
    ///
    /// A `gh run list` is a process and a network round trip of its own, and a
    /// checkout one passes through on the way to another is not a reason for
    /// either. See `ClaudhubApp::ensure_sentry` for the whole of the rule.
    pub(super) fn ensure_ci(&mut self, cx: &mut Context<Self>) {
        if self.ci.loading || self.ci.read_for == self.active {
            return;
        }
        self.load_ci(cx);
    }

    /// Reads the runs, replacing whatever was there.
    pub(super) fn load_ci(&mut self, cx: &mut Context<Self>) {
        let worktree = self.active.clone();
        self.ci = CiState {
            worktree: worktree.clone(),
            read_for: worktree.clone(),
            ..Default::default()
        };
        let Some(worktree) = worktree else {
            cx.notify();
            return;
        };
        self.ci.loading = true;
        self.ci.list_call = self.ask_gh(list_command(), worktree);
        cx.notify();
    }

    /// The CI view follows the worktree, like every other panel.
    ///
    /// It **forgets** rather than reads: the next paint of the panel is what
    /// asks for the arriving worktree's runs.
    pub(super) fn ci_follows_worktree(&mut self, cx: &mut Context<Self>) {
        if self.ci.worktree == self.active {
            return;
        }
        self.ci = CiState {
            worktree: self.active.clone(),
            ..Default::default()
        };
        cx.notify();
    }

    /// Chooses a run. It asks for nothing: a log is a second process, and one
    /// browsing the list would start one per row.
    pub(super) fn choose_ci_run(&mut self, rank: usize, cx: &mut Context<Self>) {
        if self.ci.chosen != Some(rank) {
            self.ci.log = None;
        }
        self.ci.chosen = Some(rank);
        cx.notify();
    }

    /// Asks for the chosen run's failing log — to read, or to hand over.
    pub(super) fn read_ci_log(&mut self, for_agent: bool, cx: &mut Context<Self>) {
        let (Some(worktree), Some(rank)) = (self.active.clone(), self.ci.chosen) else {
            return;
        };
        let Some(run) = self.ci.runs.get(rank) else {
            return;
        };
        let command = log_command(&run.id);
        self.ci.log_for_agent = for_agent;
        self.ci.log_call = self.ask_gh(command, worktree);
        cx.notify();
    }

    /// One of the two answers, back from the worker.
    pub(super) fn ci_answered(
        &mut self,
        call: u64,
        result: Result<String, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if call == self.ci.list_call {
            self.ci.loading = false;
            match result {
                Ok(out) => {
                    self.ci.runs = parse(&out);
                    self.ci.error = None;
                }
                Err(why) => {
                    self.ci.runs = Vec::new();
                    self.ci.error = Some(SharedString::from(why));
                }
            }
        } else if call == self.ci.log_call {
            match result {
                Ok(text) if self.ci.log_for_agent => {
                    self.hand_ci_run(&text, window, cx);
                    self.ci.log = Some(SharedString::from(tail(&text, TAIL)));
                }
                Ok(text) => self.ci.log = Some(SharedString::from(tail(&text, TAIL))),
                Err(why) => self.ci.log = Some(SharedString::from(why)),
            }
        }
        cx.notify();
    }

    /// Hands the failure to an agent, with the end of its log.
    ///
    /// The paste goes into the agent's tab: it is what has the repository in
    /// its hands. Claudhub never talks to an API for this — the same framing as
    /// the proposed commit message.
    fn hand_ci_run(&mut self, log: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(worktree), Some(rank)) = (self.active.clone(), self.ci.chosen) else {
            return;
        };
        let Some(run) = self.ci.runs.get(rank).cloned() else {
            return;
        };
        let text = format!(
            "{}\n\n{}",
            tr!("ci-prompt", { title: run.title.clone(), workflow: run.workflow.clone() }),
            tail(log, TAIL_FOR_AGENT),
        );
        self.confirm_agent_prompt(worktree, text, window, cx);
    }

    pub(super) fn render_ci(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let bar = self.render_ci_bar(cx);
        let find = self.render_find(Pane::Ci, cx);
        let query = self.query(Pane::Ci, cx);
        let muted = cx.theme().muted_foreground;

        let note = if self.active.is_none() {
            Some(tr!("no-worktree"))
        } else if self.ci.loading && self.ci.runs.is_empty() {
            Some(tr!("ci-loading"))
        } else if let Some(why) = self.ci.error.clone() {
            Some(why)
        } else {
            self.ci.runs.is_empty().then(|| tr!("ci-empty"))
        };
        if let Some(note) = note {
            return v_flex()
                .size_full()
                .child(bar)
                .children(find)
                .child(
                    v_flex()
                        .size_full()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .p_4()
                        .text_color(muted)
                        .child(icon("github"))
                        .child(div().text_sm().text_center().child(note)),
                )
                .into_any_element();
        }

        let rows: std::rc::Rc<Vec<usize>> = std::rc::Rc::new(
            self.ci
                .runs
                .iter()
                .enumerate()
                .filter(|(_, run)| run.matches(&query))
                .map(|(rank, _)| rank)
                .collect(),
        );
        let runs = std::rc::Rc::new(self.ci.runs.clone());
        let chosen = self.ci.chosen;
        let entity = cx.entity();
        let (row_height, selected, hovered) = (
            crate::ui::theme::row_height(cx) * 2.,
            cx.theme().accent,
            cx.theme().secondary,
        );
        let count = rows.len();
        v_flex()
            .size_full()
            .child(bar)
            .children(find)
            .child(
                gpui::uniform_list("ci-runs", count, {
                    let rows = rows.clone();
                    move |range, _window, _cx| {
                        range
                            .map(|row| {
                                let rank = rows[row];
                                let run = &runs[rank];
                                let app = entity.clone();
                                h_flex()
                                    .id(("ci-run", rank))
                                    .w_full()
                                    .px_2()
                                    .gap_2()
                                    .items_center()
                                    .h(row_height)
                                    .when(chosen == Some(rank), |el| el.bg(selected))
                                    .hover(|el| el.bg(hovered))
                                    .child(icon(run.glyph()).xsmall())
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .child(
                                                div()
                                                    .truncate()
                                                    .text_sm()
                                                    .child(SharedString::from(run.title.clone())),
                                            )
                                            .child(
                                                div().truncate().text_xs().text_color(muted).child(
                                                    SharedString::from(run.workflow.clone()),
                                                ),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(muted)
                                            .child(SharedString::from(run.tally().to_string())),
                                    )
                                    .on_click(move |_, _window, cx| {
                                        app.update(cx, |this, cx| this.choose_ci_run(rank, cx));
                                    })
                                    .into_any_element()
                            })
                            .collect()
                    }
                })
                .track_scroll(&self.ci.scroll)
                .flex_1()
                .min_h_0(),
            )
            .children(self.render_ci_detail(cx))
            .into_any_element()
    }

    /// What the chosen run offers. It only appears once a row is chosen: an
    /// empty section would push the list out of sight to say nothing.
    fn render_ci_detail(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let run = self.ci.runs.get(self.ci.chosen?)?.clone();
        let log = self.ci.log.clone();
        let mono = cx.theme().mono_font_family.clone();
        Some(
            v_flex()
                .gap_1()
                .p_2()
                .border_t_1()
                .border_color(cx.theme().border)
                .child(
                    div()
                        .text_sm()
                        .truncate()
                        .child(SharedString::from(run.title.clone())),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(format!(
                            "{} — {}",
                            run.workflow,
                            run.tally()
                        ))),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .child(
                            Button::new("ci-hand")
                                .primary()
                                .xsmall()
                                .icon(icon("bot"))
                                .label(tr!("ci-hand"))
                                .on_click(
                                    cx.listener(|this, _, _window, cx| this.read_ci_log(true, cx)),
                                ),
                        )
                        .child(
                            Button::new("ci-log")
                                .ghost()
                                .xsmall()
                                .icon(icon("file-text"))
                                .label(tr!("ci-log"))
                                .on_click(
                                    cx.listener(|this, _, _window, cx| this.read_ci_log(false, cx)),
                                ),
                        ),
                )
                .when_some(log, |el, log| {
                    el.child(
                        div()
                            .id("ci-log")
                            .max_h(gpui::px(220.))
                            .overflow_scroll()
                            .p_2()
                            .rounded_md()
                            .bg(cx.theme().secondary)
                            .font_family(mono)
                            .text_xs()
                            .child(log),
                    )
                }),
        )
    }

    fn render_ci_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let loading = self.ci.loading;
        let count = self.ci.runs.len();
        h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(icon("github").xsmall())
            .child(
                div()
                    .flex_1()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr!("ci-count", { n: count })),
            )
            .child(
                Button::new("ci-refresh")
                    .ghost()
                    .xsmall()
                    .icon(icon("refresh-cw"))
                    .tooltip(tr!("action-refresh"))
                    .disabled(loading)
                    .on_click(cx.listener(|this, _, _window, cx| this.load_ci(cx))),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_line_is_skipped_rather_than_failing_the_read() {
        let out = "42\tFix the thing\tCI\tcompleted\tsuccess\n\
                   oops, a notice\n\
                   43\tAnother\tRelease\tin_progress\t\n";
        let runs = parse(out);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].id, "42");
        assert_eq!(runs[0].tally(), "success");
        assert_eq!(runs[0].glyph(), "circle-check");
        // In flight: the status stands in, and the glyph says "not yet" rather
        // than "not well".
        assert_eq!(runs[1].tally(), "in_progress");
        assert_eq!(runs[1].glyph(), "loader-circle");
    }

    #[test]
    fn a_failure_draws_the_cross() {
        let runs = parse("7\tt\tw\tcompleted\tfailure\n");
        assert_eq!(runs[0].glyph(), "circle-x");
    }

    #[test]
    fn the_tail_is_the_end_and_a_short_log_is_itself() {
        assert_eq!(tail("a\nb\nc", 2), "b\nc");
        assert_eq!(tail("a\nb", 5), "a\nb");
        assert_eq!(tail("", 5), "");
    }

    /// The trap that costs a click: a Go template writes a JSON number as a
    /// float, and `3.2494024323e+10` is an id `gh run view` does not know.
    #[test]
    fn the_listing_asks_for_the_id_as_an_integer() {
        assert!(
            list_command().contains(r#"printf "%.0f" .databaseId"#),
            "{}",
            list_command()
        );
        assert!(log_command("42").contains("--log-failed"));
    }

    #[test]
    fn the_filter_reads_the_title_and_the_workflow() {
        let runs = parse("42\tFix the thing\tCI\tcompleted\tsuccess\n");
        assert!(runs[0].matches("thing"));
        assert!(runs[0].matches("ci"));
        assert!(!runs[0].matches("42"));
        assert!(runs[0].matches(" "));
    }
}
