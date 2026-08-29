//! The two Sentry views: the errors of a project, and the one being read.
//!
//! **Two panels on one state**, which is the gesture of the rest of the window
//! — the project tree and the editor, the hits and the preview: choosing an
//! error must not push out of sight the list one is choosing from. The list is
//! a tool window against the left edge, the error is a document of the centre,
//! and it is the arrangement Sentry's own page has.
//!
//! What is decided lives in `crate::sentry`, pure and tested: the URLs, the
//! shapes the API returns, the filter, the prompt. This module holds what it
//! takes to paint them and the three round trips that fill them.
//!
//! **The project belongs to the repository** and the organisation to the
//! machine: five checkouts of one code have the same errors, and two
//! repositories of one organisation do not. That is why the project is in the
//! store and everything else in the settings — and why the field for it is on
//! the panel rather than in a settings page one would have to go and find.

use std::path::PathBuf;

use gpui::{div, prelude::*, px, App, Context, Pixels, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::Input,
    v_flex, ActiveTheme, Disableable as _, Sizable as _, WindowExt as _,
};

use crate::runtime::protocol::Caller;
use crate::runtime::Cmd;
use crate::sentry::{Event, Issue, Spread};
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::find::Pane;
use crate::ui::icons::icon;
use crate::ui::settings::Settings;

/// What the two views show and what they are waiting for.
///
/// One state for the window and not one per repository: what it holds is a
/// reading of one project, and arriving somewhere else is starting over rather
/// than refreshing — the rule every panel of this window lives by.
#[derive(Default)]
pub struct SentryState {
    /// The repository this reading is about, which is what says it is stale.
    pub main: Option<PathBuf>,
    pub issues: Vec<Issue>,
    /// The rank in `issues` — **not** in the filtered rows: a filter changes
    /// which row is which, and what one is reading must survive a keystroke.
    pub chosen: Option<usize>,
    pub event: Option<Event>,
    pub spreads: Vec<Spread>,
    pub loading: bool,
    /// Why the list is empty, when it is not simply empty.
    pub error: Option<SharedString>,
    /// Why the trace is missing, which is not why the list is.
    pub event_error: Option<SharedString>,
    /// What the reading on screen was made for: the account and the project,
    /// as one string.
    ///
    /// **A signature and not a flag**, and the difference is the trap it
    /// exists for: with a flag, filling in the token in the settings and coming
    /// back left the panel saying "set the organisation and the token" for
    /// ever, since it had already read once. It also stands in for the flag —
    /// `None` is "never read" — and an account not set up yet signs as the
    /// empty string, which is a reading that happened and must not be redone
    /// every frame.
    pub read_for: Option<String>,
    /// The sections one has folded, by the key `Section::key` gives.
    ///
    /// **What is folded and not what is open**, so a section added later shows
    /// rather than hides — and the distribution, folded to start with, says so
    /// by being in here from the first frame. In memory like the notes' folds.
    pub folded: std::collections::BTreeSet<&'static str>,
    /// The sends in flight. A late answer is dropped rather than shown: one
    /// changes error before the previous trace has come back, and painting it
    /// would replace what is being read with what is not.
    pub list_call: u64,
    pub event_call: u64,
    pub tags_call: u64,
    pub scroll: gpui::UniformListScrollHandle,
    /// The error's own scroll: a trace is read by scrolling, and the list one
    /// picked it from is not what moves under the wheel.
    pub issue_scroll: gpui::UniformListScrollHandle,
    /// The body of the error's page, laid out. Built when its key moves and
    /// kept otherwise: a frame spent only scrolling then costs a slice of a
    /// `Vec` rather than a grammar pass per excerpt.
    pub page: Option<Page>,
}

/// A foldable block of the error's page.
///
/// Named rather than numbered: the set of folds outlives the error one is
/// reading — one folds the distribution once and means it for the next error
/// too — and a rank would move the moment a section has nothing to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Context,
    Trace,
    Spread,
    Crumbs,
}

impl Section {
    /// What the fold set files it under, and the i18n key its heading is named
    /// by — one name, so a heading and its fold cannot drift apart.
    pub fn key(self) -> &'static str {
        match self {
            Self::Context => "sentry-context",
            Self::Trace => "sentry-trace",
            Self::Spread => "sentry-spread",
            Self::Crumbs => "sentry-crumbs",
        }
    }

    /// **The distribution starts folded, and it is the reason folds exist
    /// here.** It is proportions one looks at once — is this bug everywhere or
    /// at one customer — and seven tags unfolded push the trace, which is what
    /// one came to read, off the bottom of the screen.
    fn folded_at_birth(self) -> bool {
        matches!(self, Self::Spread)
    }

    const ALL: [Section; 4] = [Self::Context, Self::Trace, Self::Spread, Self::Crumbs];
}

impl SentryState {
    /// The issue being read, if the list still holds it.
    pub fn issue(&self) -> Option<&Issue> {
        self.issues.get(self.chosen?)
    }

    /// The folds a fresh reading opens with.
    pub fn fresh_folds() -> std::collections::BTreeSet<&'static str> {
        Section::ALL
            .into_iter()
            .filter(|section| section.folded_at_birth())
            .map(Section::key)
            .collect()
    }

    pub fn is_folded(&self, section: Section) -> bool {
        self.folded.contains(section.key())
    }
}

impl ClaudhubApp {
    /// The organisation, the host and the token, as the settings hold them.
    ///
    /// `None` when the two that matter are not both there: a request with no
    /// organisation is a 404, and one with no token is a 401 — neither says
    /// anything the panel could not say first.
    fn sentry_account(&self, cx: &App) -> Option<(String, String, crate::runtime::Secret)> {
        let settings = Settings::global(cx);
        let org = settings.sentry_org.trim().to_string();
        // **Resolved here and not in the worker**, and only for the keyring
        // form: a keyring belongs to a desktop session, which is the Windows
        // side when the workers live in WSL. `$NAME` travels as it stands and
        // is read in the worker's environment, which is where the request is
        // made. See `ui::keyring`.
        let token = crate::ui::keyring::resolve(&settings.sentry_token)?;
        if org.is_empty() {
            return None;
        }
        let host = match settings.sentry_host.trim() {
            "" => crate::sentry::DEFAULT_HOST.to_string(),
            host => host.to_string(),
        };
        Some((org, host, crate::runtime::Secret(token)))
    }

    /// The project this repository's errors are read from.
    pub(super) fn sentry_project(&self, cx: &App) -> Option<String> {
        let main = self.active_main()?;
        crate::ui::store::Store::global(cx)
            .repos
            .get(&main)
            .and_then(|repo| repo.sentry_project.clone())
            .map(|project| project.trim().to_string())
            .filter(|project| !project.is_empty())
    }

    /// One request of Sentry, and the number that will carry its answer home.
    fn ask_sentry(&mut self, url: String, token: crate::runtime::Secret) -> u64 {
        self.sentry_seq += 1;
        let call = self.sentry_seq;
        self.git.send(Cmd::Call {
            caller: Caller::Sentry,
            call,
            cap: crate::outside::Cap::Http {
                method: "GET".into(),
                url,
                // The token is **not** written into the header here: `{secret}`
                // is replaced in the worker, so it never reaches something a
                // `Debug` prints. See `outside::Cap`.
                headers: vec![("Authorization".into(), "Bearer {secret}".into())],
                body: None,
                secret: Some(token),
            },
        });
        call
    }

    /// Reads the project's issues **the first time the panel is drawn**.
    ///
    /// A panel that is drawn is a panel on screen: the tab is displayed, the
    /// zone is unfolded, and the view is not put away. That is the whole of the
    /// condition, and it is read off the frame rather than tracked — the rule
    /// the history's own loading already lives by.
    ///
    /// It matters more here than there: reading a `git log` of a worktree
    /// nobody is looking at wastes a fork, and reading Sentry's wastes a round
    /// trip to somebody else's server, on every checkout one passes through.
    ///
    /// Asked **once per account**, or every frame the panel is up would restart
    /// it — and never again for the same one, or a token corrected in the
    /// settings would never take effect.
    pub(super) fn ensure_sentry(&mut self, cx: &mut Context<Self>) {
        if self.sentry.loading || self.sentry.read_for.as_deref() == Some(&self.sentry_key(cx)) {
            return;
        }
        self.load_sentry(cx);
    }

    /// The account and the project this reading is about, as one string.
    ///
    /// Empty when there is nothing to read with, which is a state like any
    /// other: it is what the panel says in words, and what changes the moment
    /// one fills the settings in.
    fn sentry_key(&self, cx: &App) -> String {
        let Some((org, host, _)) = self.sentry_account(cx) else {
            return String::new();
        };
        match self.sentry_project(cx) {
            Some(project) => format!("{org}/{project}@{host}"),
            None => String::new(),
        }
    }

    /// Reads the project's issues, replacing whatever was there.
    ///
    /// Called from the panel's first paint and from its refresh button. It is a
    /// **replacement** and not a refresh: the errors of another project have
    /// nothing to do with this one's.
    pub(super) fn load_sentry(&mut self, cx: &mut Context<Self>) {
        let main = self.active_main();
        let key = self.sentry_key(cx);
        self.sentry = SentryState {
            main: main.clone(),
            folded: SentryState::fresh_folds(),
            read_for: Some(key),
            ..Default::default()
        };
        let Some((org, host, token)) = self.sentry_account(cx) else {
            cx.notify();
            return;
        };
        let Some(project) = self.sentry_project(cx) else {
            cx.notify();
            return;
        };
        let query = match Settings::global(cx).sentry_query.trim() {
            "" => crate::sentry::DEFAULT_QUERY.to_string(),
            query => query.to_string(),
        };
        let url = crate::sentry::issues_url(&host, &org, &project, &query);
        self.sentry.loading = true;
        self.sentry.list_call = self.ask_sentry(url, token);
        cx.notify();
    }

    /// The Sentry views follow the worktree, like every other panel.
    ///
    /// It **forgets** rather than reads: what is on screen belongs to the
    /// repository one has just left, and the next paint of the panel is what
    /// asks for this one's. A checkout one passes through on the way to another
    /// therefore costs no request at all.
    ///
    /// The project's field is refilled, though: it belongs to the repository,
    /// so leaving the previous one's name under the caret would be the one
    /// thing a per-repository setting must not do.
    pub(super) fn sentry_follows_worktree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sentry.main == self.active_main() {
            return;
        }
        let project = self.sentry_project(cx).unwrap_or_default();
        let input = self.sentry_project_input.clone();
        input.update(cx, |input, cx| input.set_value(project, window, cx));
        self.sentry = SentryState {
            main: self.active_main(),
            folded: SentryState::fresh_folds(),
            ..Default::default()
        };
        cx.notify();
    }

    /// The project's field validates on Enter and on losing the focus.
    ///
    /// Losing the focus validates, which is already this window's rule for the
    /// task list: `InputState` has no escape event, and throwing away what was
    /// typed because one clicked beside it is the worse of the two defaults.
    /// Nothing is asked of Sentry per keystroke — a request per letter of a
    /// project's name.
    pub(super) fn watch_sentry_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.sentry_project_input.clone();
        cx.subscribe_in(&input, window, |this, input, event, _window, cx| {
            use gpui_component::input::InputEvent;
            if !matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                return;
            }
            let typed = input.read(cx).value().to_string();
            if this.sentry_project(cx).unwrap_or_default() == typed.trim() {
                return;
            }
            this.set_sentry_project(typed, cx);
        })
        .detach();
    }

    /// Files the project this repository's errors come from.
    pub(super) fn set_sentry_project(&mut self, project: String, cx: &mut Context<Self>) {
        let Some(main) = self.active_main() else {
            return;
        };
        let project = project.trim().to_string();
        crate::ui::store::Store::update_global(cx, |store| {
            store.repos.entry(main).or_default().sentry_project =
                (!project.is_empty()).then_some(project.clone());
        });
        self.load_sentry(cx);
    }

    /// Opens one error: its trace, and what its tags are worth.
    ///
    /// Two round trips, and the second's failure says nothing more than "no
    /// bars": the trace, which is what one came for, has arrived.
    pub(super) fn open_sentry_issue(
        &mut self,
        rank: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(issue) = self.sentry.issues.get(rank) else {
            return;
        };
        let id = issue.id.clone();
        self.sentry.chosen = Some(rank);
        self.sentry.folded = SentryState::fresh_folds();
        self.sentry.event = None;
        self.sentry.event_error = None;
        self.sentry.spreads = Vec::new();
        if let Some((org, host, token)) = self.sentry_account(cx) {
            let event = crate::sentry::event_url(&host, &org, &id);
            let tags = crate::sentry::tags_url(&host, &org, &id);
            self.sentry.event_call = self.ask_sentry(event, token.clone());
            self.sentry.tags_call = self.ask_sentry(tags, token);
        }
        // The centre's tab appears with the error and comes forward: it is the
        // console's rule, and what makes "choose a row" one gesture.
        self.reveal_panel(crate::ui::panels::SentryIssuePanel::NAME, window, cx);
        cx.notify();
    }

    /// Folds a section away, or gives it back.
    pub(super) fn fold_sentry_section(&mut self, section: Section, cx: &mut Context<Self>) {
        if !self.sentry.folded.remove(section.key()) {
            self.sentry.folded.insert(section.key());
        }
        cx.notify();
    }

    /// Whether the centre has an error to show — what its tab hangs on.
    pub(super) fn sentry_issue_open(&self) -> bool {
        self.sentry.chosen.is_some()
    }

    /// The cross on the error's tab: done with this one.
    pub(super) fn close_sentry_issue(&mut self, cx: &mut Context<Self>) {
        self.sentry.chosen = None;
        self.sentry.event = None;
        self.sentry.event_error = None;
        self.sentry.spreads = Vec::new();
        cx.notify();
    }

    /// One of the three answers, back from the worker.
    pub(super) fn sentry_answered(
        &mut self,
        call: u64,
        result: Result<String, String>,
        cx: &mut Context<Self>,
    ) {
        if call == self.sentry.list_call {
            self.sentry.loading = false;
            match result.and_then(|body| {
                crate::sentry::parse_issues(&body).map_err(|why| format!("{why:#}"))
            }) {
                Ok(issues) => {
                    self.sentry.issues = issues;
                    self.sentry.error = None;
                }
                Err(why) => {
                    self.sentry.issues = Vec::new();
                    self.sentry.error = Some(SharedString::from(why));
                }
            }
        } else if call == self.sentry.event_call {
            match result.and_then(|body| {
                crate::sentry::parse_event(&body).map_err(|why| format!("{why:#}"))
            }) {
                Ok(Some(event)) => self.sentry.event = Some(event),
                Ok(None) => self.sentry.event_error = Some(tr!("sentry-event-expired")),
                Err(why) => self.sentry.event_error = Some(SharedString::from(why)),
            }
        } else if call == self.sentry.tags_call {
            // Its failure is not worth a line on screen: the bars are the one
            // reading of this page one can do without.
            match result
                .and_then(|body| crate::sentry::parse_tags(&body).map_err(|why| format!("{why:#}")))
            {
                Ok(spreads) => self.sentry.spreads = spreads,
                Err(why) => log::warn!("sentry tags: {why}"),
            }
        }
        cx.notify();
    }

    /// Hands the error to an agent, with the code around our own frames.
    pub(super) fn hand_sentry_issue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(issue) = self.sentry.issue().cloned() else {
            return;
        };
        let intro = match Settings::global(cx).sentry_intro.trim() {
            "" => tr!("sentry-intro").to_string(),
            intro => intro.to_string(),
        };
        let org = Settings::global(cx).sentry_org.trim().to_string();
        let text =
            crate::sentry::prompt(&intro, &org, &issue, self.sentry.event.as_ref(), &worktree);
        // Through the terminal, in a bracketed paste, like the notes: the agent
        // is what has the repository in its hands, and Claudhub never talks to
        // an API for this.
        self.confirm_agent_prompt(worktree, text, window, cx);
    }

    /// Shows what is about to be handed to an agent, and lets it be edited.
    ///
    /// **The notes' own dialog, on the same field** (`prompt_input`): what goes
    /// into a terminal cannot be taken back — an agent has read the paste
    /// before one has seen what one just sent — and the two gestures are the
    /// same gesture.
    ///
    /// Here it earns a second reason. What this writes is a **report**, not a
    /// request: a Sentry issue arrives with its trace, its context and its
    /// code, and what one wants to add is the one sentence that narrows it —
    /// start with the controller, leave the migrations alone, this only happens
    /// in production. That sentence has nowhere else to be written.
    ///
    /// An empty field sends nothing: emptying it is how one changes one's mind
    /// once the dialog is open.
    pub(super) fn confirm_agent_prompt(
        &mut self,
        worktree: PathBuf,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = self.prompt_input.clone();
        let entity = cx.entity();
        input.update(cx, |input, cx| input.set_value(text, window, cx));
        window.open_dialog(cx, move |dialog, _window, _cx| {
            // Cloned into the closure and never read from it: `open_dialog`
            // keeps a `Fn` called back from the root's own render, where
            // reading the application is a panic. See "Conventions gpui".
            let input = input.clone();
            let entity = entity.clone();
            let worktree = worktree.clone();
            dialog
                .title(tr!("agent-prompt-title"))
                .child(
                    v_flex()
                        .gap_2()
                        .w(gpui::px(640.))
                        .child(div().text_xs().child(tr!("agent-prompt-hint")))
                        .child(gpui_component::input::Textarea::new(&input)),
                )
                .overlay_closable(false)
                .close_button(false)
                .footer(crate::ui::dialogs::confirm())
                .on_ok(move |_, window, cx| {
                    let text = input.read(cx).value().to_string();
                    if text.trim().is_empty() {
                        return true;
                    }
                    entity.update(cx, |this, cx| {
                        // Shown before it is sent: a message delivered into a
                        // hidden tab is a message nobody sees arrive. It is
                        // what the notes' `deliver` does, and for the same
                        // reason.
                        this.show_terminal_panel(window, cx);
                        this.send_to_agent(&worktree, text, window, cx);
                    });
                    true
                })
        });
        // The text is already there and it is meant to be added to: the caret
        // goes in the field.
        crate::ui::dialogs::focus_field(&self.prompt_input, window, cx);
    }
}

// — The list, against the left edge ——————————————————————————————————

impl ClaudhubApp {
    pub(super) fn render_sentry(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let bar = self.render_sentry_bar(window, cx);
        let find = self.render_find(Pane::Sentry, cx);
        let query = self.query(Pane::Sentry, cx);
        let muted = cx.theme().muted_foreground;

        // The rows are **indices** into the list, as the stashes' are: a frame
        // costs no copy of an issue.
        let rows: std::rc::Rc<Vec<usize>> = std::rc::Rc::new(
            self.sentry
                .issues
                .iter()
                .enumerate()
                .filter(|(_, issue)| issue.matches(&query))
                .map(|(rank, _)| rank)
                .collect(),
        );
        let note = self.sentry_note(cx);
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
                        .child(icon("triangle-alert"))
                        .child(div().text_sm().text_center().child(note)),
                )
                .into_any_element();
        }

        let issues = std::rc::Rc::new(self.sentry.issues.clone());
        let chosen = self.sentry.chosen;
        let scroll = self.sentry.scroll.clone();
        let count = rows.len();
        let total = issues.len();
        let entity = cx.entity();
        let look = Look::of(cx);
        v_flex()
            .size_full()
            .child(bar)
            .children(find)
            .child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(muted)
                    .child(if count == total {
                        tr!("sentry-count", { n: total })
                    } else {
                        tr!("sentry-count-filtered", { n: count, total: total })
                    }),
            )
            .child(
                gpui::uniform_list("sentry-issues", count, {
                    let rows = rows.clone();
                    move |range, _window, cx| {
                        range
                            .map(|row| {
                                let rank = rows[row];
                                let _ = cx;
                                sentry_row(
                                    &issues[rank],
                                    rank,
                                    chosen == Some(rank),
                                    &look,
                                    &entity,
                                )
                            })
                            .collect()
                    }
                })
                .track_scroll(&scroll)
                .flex_1()
                .min_h_0(),
            )
            .into_any_element()
    }

    /// Why the list shows nothing, when that is not simply "no errors".
    ///
    /// **The sentence says what to do**, and the field to do it with is the one
    /// above: a panel that reports a missing setting without offering it is a
    /// panel one leaves to go looking through a settings page.
    fn sentry_note(&self, cx: &App) -> Option<SharedString> {
        if self.active_main().is_none() {
            return Some(tr!("no-worktree"));
        }
        if self.sentry_account(cx).is_none() {
            return Some(tr!("sentry-no-account"));
        }
        if self.sentry_project(cx).is_none() {
            return Some(tr!("sentry-no-project"));
        }
        if let Some(why) = self.sentry.error.clone() {
            return Some(why);
        }
        if self.sentry.loading && self.sentry.issues.is_empty() {
            return Some(tr!("sentry-loading"));
        }
        self.sentry.issues.is_empty().then(|| tr!("sentry-empty"))
    }

    /// The organisation, the project, and a way to read again.
    fn render_sentry_bar(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let org = Settings::global(cx).sentry_org.trim().to_string();
        let input = self.sentry_project_input.clone();
        let loading = self.sentry.loading;
        h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(icon("triangle-alert").xsmall())
            .when(!org.is_empty(), |el| {
                el.child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(org)),
                )
            })
            // **The project's field is always there**, and not only when
            // nothing works: it is the one setting of these views that belongs
            // to the repository, one corrects it as often as one sets it, and a
            // field one has to break something to find is not a field.
            .child(div().flex_1().child(Input::new(&input).xsmall()))
            .child(self.find_button(Pane::Sentry, cx))
            .child(
                Button::new("sentry-refresh")
                    .ghost()
                    .xsmall()
                    .icon(icon("refresh-cw"))
                    .tooltip(tr!("action-refresh"))
                    .disabled(loading)
                    .on_click(cx.listener(|this, _, _window, cx| this.load_sentry(cx))),
            )
    }
}

/// What a row's paint needs, read once per frame rather than per row.
struct Look {
    row: gpui::Pixels,
    muted: gpui::Hsla,
    selected: gpui::Hsla,
    hovered: gpui::Hsla,
}

impl Look {
    fn of(cx: &App) -> Self {
        Self {
            // Two storeys: what the error says, then where and when.
            row: crate::ui::theme::row_height(cx) * 2.,
            muted: cx.theme().muted_foreground,
            selected: cx.theme().accent,
            hovered: cx.theme().secondary,
        }
    }
}

/// One row of the list: what it is, where, and how often.
fn sentry_row(
    issue: &Issue,
    rank: usize,
    selected: bool,
    look: &Look,
    app: &gpui::Entity<ClaudhubApp>,
) -> gpui::AnyElement {
    let app = app.clone();
    let last = when(&issue.last_seen);
    let subtitle = [issue.culprit.as_str(), last.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    // A band and not a pill: what a selected row names is the **row**. See
    // "Le grain de l'interface".
    h_flex()
        .id(("sentry-row", rank))
        .w_full()
        .px_2()
        .py_1()
        .gap_2()
        .items_center()
        .h(look.row)
        .when(selected, |el| el.bg(look.selected))
        .hover(|el| el.bg(look.hovered))
        .child(icon(level_icon(&issue.level)).xsmall())
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().truncate().text_sm().child(issue.title.clone()))
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(look.muted)
                        .child(SharedString::from(subtitle)),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(look.muted)
                .child(SharedString::from(issue.count.to_string())),
        )
        .on_click(move |_, window, cx| {
            app.update(cx, |this, cx| this.open_sentry_issue(rank, window, cx));
        })
        .into_any_element()
}

/// The glyph a level draws. Unknown levels get the neutral one rather than
/// none: Sentry's list is open, and a row with no icon reads as a broken row.
fn level_icon(level: &str) -> &'static str {
    match level {
        "fatal" | "error" => "circle-x",
        "warning" => "triangle-alert",
        _ => "info",
    }
}

// — The error being read, in the centre ——————————————————————————————

/// One line of the error's body, as the virtual list counts it.
///
/// **Everything below the header is one line tall**, code included, which is
/// what lets a `uniform_list` carry it. A stack of two hundred frames is two
/// thousand lines of code, and painting them all — colouring them all — is what
/// made the wheel crawl.
#[derive(Clone)]
pub enum Row {
    /// A section's heading, which is also what folds it.
    Section(Section, SharedString),
    /// A key and its value: a tag of the event, or a breadcrumb.
    Pair(SharedString, SharedString),
    /// The name of one tag of the distribution.
    SpreadName(SharedString),
    /// One value's share of it.
    Bar(SharedString, u8),
    /// A frame's heading: its path, its function, its badge.
    Frame(usize),
    /// One line of a frame's excerpt.
    Code(usize, usize),
}

/// A frame as the page paints it: its text ready, and its colours **worked out
/// once**.
///
/// Colouring is tens of milliseconds of grammar work, and it was being done for
/// every frame of every paint — which is the other half of why scrolling a long
/// trace crawled. It is done here, when the event lands, and kept until the
/// page's key moves.
pub struct PaintedFrame {
    pub path: String,
    pub line: usize,
    pub function: String,
    pub in_app: bool,
    /// The file exists in this worktree, so the heading opens it.
    pub opens: bool,
    pub context: Vec<(usize, SharedString)>,
    /// One entry per line of `context`; empty where nothing colours it.
    pub styles: Vec<Vec<(std::ops::Range<usize>, gpui::HighlightStyle)>>,
}

/// What the body was laid out for. It moves, the body is built again.
#[derive(PartialEq, Eq)]
pub struct PageKey {
    issue: String,
    /// The event is there. Its arrival is what fills the trace.
    event: bool,
    spreads: usize,
    folded: std::collections::BTreeSet<&'static str>,
    /// The theme, because the colours are baked into the rows.
    theme: String,
    worktree: Option<PathBuf>,
}

/// The body of the page, laid out.
pub struct Page {
    key: PageKey,
    rows: std::rc::Rc<Vec<Row>>,
    frames: std::rc::Rc<Vec<PaintedFrame>>,
}

impl ClaudhubApp {
    /// Lays the body out, when what it is made of has moved.
    ///
    /// The same device as the SQL history's list: a key of everything the
    /// layout reads, and nothing rebuilt while it holds. A frame of a page one
    /// is only scrolling then costs a slice of a `Vec`.
    fn refresh_sentry_page(&mut self, cx: &App) {
        let settings = Settings::global(cx);
        let key = PageKey {
            issue: self
                .sentry
                .issue()
                .map(|issue| issue.id.clone())
                .unwrap_or_default(),
            event: self.sentry.event.is_some(),
            spreads: self.sentry.spreads.len(),
            folded: self.sentry.folded.clone(),
            theme: format!(
                "{:?}/{}/{}",
                settings.theme, settings.light_theme, settings.dark_theme
            ),
            worktree: self.active.clone(),
        };
        if self
            .sentry
            .page
            .as_ref()
            .is_some_and(|page| page.key == key)
        {
            return;
        }
        let worktree = self.active.clone().unwrap_or_default();
        let highlight = cx.theme().highlight_theme.clone();
        let mut rows = Vec::new();
        let mut frames = Vec::new();

        if let Some(event) = self.sentry.event.clone() {
            // **The context, then the trace.** The trace is what one came for,
            // so nothing that can run to seven blocks goes above it — the
            // distribution used to, and it pushed the trace off the screen.
            if !event.tags.is_empty() {
                rows.push(Row::Section(Section::Context, tr!("sentry-context")));
                if !self.sentry.is_folded(Section::Context) {
                    for tag in &event.tags {
                        rows.push(Row::Pair(
                            SharedString::from(tag.key.clone()),
                            SharedString::from(tag.value.clone()),
                        ));
                    }
                }
            }
            if !event.frames.is_empty() {
                rows.push(Row::Section(
                    Section::Trace,
                    tr!("sentry-trace", { n: event.frames.len() }),
                ));
                // **Newest first.** Sentry's order is the call's — oldest first
                // — and what one comes for is the line that raised.
                for frame in event.frames.iter().rev() {
                    let path = frame.repo_path(&worktree);
                    // The excerpt is a fragment with nothing before it — a
                    // grammar's error recovery is what makes parsing it on its
                    // own worth doing — and its language comes from the path
                    // rather than being guessed from the text, which is how a
                    // shell transcript ends up painted as Rust.
                    let source: String = frame
                        .context
                        .iter()
                        .map(|(_, text)| text.as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    let styles = crate::ui::highlight::language_for_path(std::path::Path::new(
                        &path,
                    ))
                    .map(|language| {
                        crate::ui::highlight::DocumentHighlights::for_language(
                            language, &source, &highlight,
                        )
                    });
                    let at = frames.len();
                    rows.push(Row::Frame(at));
                    if !self.sentry.is_folded(Section::Trace) {
                        for index in 0..frame.context.len() {
                            rows.push(Row::Code(at, index));
                        }
                    }
                    frames.push(PaintedFrame {
                        opens: !path.is_empty() && worktree.join(&path).exists(),
                        line: frame.line,
                        function: frame.function.clone(),
                        in_app: frame.in_app,
                        context: frame
                            .context
                            .iter()
                            .map(|(number, text)| (*number, SharedString::from(text.clone())))
                            .collect(),
                        styles: (0..frame.context.len())
                            .map(|index| {
                                styles
                                    .as_ref()
                                    .map(|styles| styles.line(index).to_vec())
                                    .unwrap_or_default()
                            })
                            .collect(),
                        path,
                    });
                }
                // A folded trace keeps no frame rows, headings included: what
                // one folded away is the stack, not the code inside it.
                if self.sentry.is_folded(Section::Trace) {
                    rows.retain(|row| !matches!(row, Row::Frame(_)));
                }
            }
            if !self.sentry.spreads.is_empty() {
                rows.push(Row::Section(Section::Spread, tr!("sentry-spread")));
                if !self.sentry.is_folded(Section::Spread) {
                    for spread in &self.sentry.spreads {
                        rows.push(Row::SpreadName(SharedString::from(spread.name.clone())));
                        for (value, share) in &spread.values {
                            rows.push(Row::Bar(SharedString::from(value.clone()), *share));
                        }
                    }
                }
            }
            if !event.crumbs.is_empty() {
                rows.push(Row::Section(
                    Section::Crumbs,
                    tr!("sentry-crumbs", { n: event.crumbs.len() }),
                ));
                if !self.sentry.is_folded(Section::Crumbs) {
                    for crumb in &event.crumbs {
                        rows.push(Row::Pair(
                            SharedString::from(crumb.category.clone()),
                            SharedString::from(crumb.message.clone()),
                        ));
                    }
                }
            }
        }
        self.sentry.page = Some(Page {
            key,
            rows: std::rc::Rc::new(rows),
            frames: std::rc::Rc::new(frames),
        });
    }

    /// Its trace, the deployed code, what is known of it, and the gesture.
    ///
    /// This is the half that needs width — an excerpt of code and a stack of
    /// paths — and it is what a single panel could not give.
    ///
    /// **The header stays and the body scrolls.** A trace two hundred frames
    /// deep is read by scrolling, and what one is reading has to keep saying
    /// which error it is; the body is a `uniform_list`, so the frame's cost is
    /// what fits on screen rather than what the stack holds.
    pub(super) fn render_sentry_issue(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(issue) = self.sentry.issue().cloned() else {
            return div().into_any_element();
        };
        self.refresh_sentry_page(cx);
        let muted = cx.theme().muted_foreground;
        let mono = cx.theme().mono_font_family.clone();
        let code_size = px(Settings::global(cx).diff_font_size);
        let line = crate::ui::diff_view::line_height(code_size);
        let event_error = self.sentry.event_error.clone();
        let waiting = self.sentry.event.is_none() && event_error.is_none();
        let permalink = issue.permalink.clone();
        let short_id = issue.short_id.clone();

        let head = v_flex()
            .gap_1()
            .p_3()
            .child(div().text_lg().child(issue.kind.clone()))
            .when(!issue.value.is_empty(), |el| {
                el.child(div().text_sm().child(issue.value.clone()))
            })
            .when(!issue.culprit.is_empty(), |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(issue.culprit.clone()),
                )
            })
            // What one reads before reading anything: how bad, and whether
            // somebody has already dealt with it.
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(sentry_badge(&issue.level, level_tone(&issue.level, cx)))
                    .when(!issue.status.is_empty(), |el| {
                        el.child(sentry_badge(&issue.status, status_tone(&issue.status, cx)))
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .pt_1()
                    .child(sentry_pair(
                        tr!("sentry-events"),
                        issue.count.to_string(),
                        cx,
                    ))
                    .when(issue.users > 0, |el| {
                        el.child(sentry_pair(
                            tr!("sentry-users"),
                            issue.users.to_string(),
                            cx,
                        ))
                    })
                    .when(!issue.first_seen.is_empty(), |el| {
                        el.child(sentry_pair(
                            tr!("sentry-first"),
                            when(&issue.first_seen),
                            cx,
                        ))
                    })
                    .when(!issue.last_seen.is_empty(), |el| {
                        el.child(sentry_pair(tr!("sentry-last"), when(&issue.last_seen), cx))
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .pt_1()
                    .child(
                        Button::new("sentry-hand")
                            .primary()
                            .small()
                            .icon(icon("bot"))
                            .label(tr!("sentry-hand"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.hand_sentry_issue(window, cx)
                            })),
                    )
                    // **A button and not a text**: the short id is the
                    // reference one carries elsewhere — a branch name, a commit
                    // message, a message to somebody — and a panel has no text
                    // selection to take it from by hand.
                    .when(!short_id.is_empty(), |el| {
                        let id = short_id.clone();
                        el.child(
                            Button::new("sentry-copy-id")
                                .ghost()
                                .small()
                                .icon(icon("copy"))
                                .label(SharedString::from(short_id.clone()))
                                .on_click(cx.listener(move |this, _, _window, cx| {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                        id.clone(),
                                    ));
                                    this.announce(tr!("sentry-id-copied", { id: id.clone() }), cx);
                                })),
                        )
                    })
                    .when(!permalink.is_empty(), |el| {
                        let url = permalink.clone();
                        el.child(
                            Button::new("sentry-open")
                                .ghost()
                                .small()
                                .icon(icon("external-link"))
                                .label(tr!("sentry-open-in-sentry"))
                                .on_click(move |_, _window, cx| cx.open_url(&url)),
                        )
                    }),
            );

        // Under the header and not in place of the panel: the reference and the
        // counters have done nothing wrong.
        if let Some(why) = event_error {
            return v_flex()
                .size_full()
                .child(head)
                .child(
                    div()
                        .p_3()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(why),
                )
                .into_any_element();
        }
        if waiting {
            return v_flex()
                .size_full()
                .child(head)
                .child(
                    div()
                        .p_3()
                        .text_sm()
                        .text_color(muted)
                        .child(tr!("sentry-loading-event")),
                )
                .into_any_element();
        }

        let Some(page) = self.sentry.page.as_ref() else {
            return v_flex().size_full().child(head).into_any_element();
        };
        let (rows, frames) = (page.rows.clone(), page.frames.clone());
        let entity = cx.entity();
        let handle = self.sentry.issue_scroll.clone();
        let look = CodeLook {
            line,
            mono,
            muted,
            code: code_size,
            marked: cx.theme().warning.opacity(0.15),
            ground: cx.theme().secondary,
            info: cx.theme().info,
            primary: cx.theme().primary,
            folded: cx.theme().muted,
        };
        let count = rows.len();
        v_flex()
            .size_full()
            .child(head)
            .child(
                div().flex_1().min_h_0().px_3().child(
                    // Wheel smoothing, as everywhere else: a trace runs to
                    // hundreds of lines, and a notch jumping three at once
                    // makes the eye lose its place.
                    self.scrolled(
                        "sentry-issue",
                        &handle,
                        crate::ui::motion::Axes::Vertical,
                        window,
                        gpui::uniform_list("sentry-rows", count, move |range, _window, _cx| {
                            range
                                .map(|index| sentry_row_of(&rows[index], &frames, &look, &entity))
                                .collect::<Vec<_>>()
                        })
                        .size_full()
                        .track_scroll(&handle),
                        cx,
                    ),
                ),
            )
            .into_any_element()
    }
}

/// What painting one line of the body needs, read once per frame.
struct CodeLook {
    line: Pixels,
    mono: SharedString,
    muted: gpui::Hsla,
    code: Pixels,
    marked: gpui::Hsla,
    ground: gpui::Hsla,
    info: gpui::Hsla,
    primary: gpui::Hsla,
    folded: gpui::Hsla,
}

/// One line of the body.
///
/// Every arm is **one line tall**, explicitly: a `uniform_list` reserves what
/// it is told, and a line that overflows what it reserved is a line drawn over
/// its neighbour.
fn sentry_row_of(
    row: &Row,
    frames: &std::rc::Rc<Vec<PaintedFrame>>,
    look: &CodeLook,
    app: &gpui::Entity<ClaudhubApp>,
) -> gpui::AnyElement {
    let base = || h_flex().h(look.line).w_full().items_center().gap_2();
    match row {
        Row::Section(section, title) => {
            let (section, app) = (*section, app.clone());
            base()
                .id(SharedString::new_static(section.key()))
                .text_xs()
                .text_color(look.muted)
                .child(title.clone())
                .on_click(move |_, _window, cx| {
                    app.update(cx, |this, cx| this.fold_sentry_section(section, cx));
                })
                .into_any_element()
        }
        Row::Pair(key, value) => base()
            .text_xs()
            .child(div().text_color(look.muted).child(key.clone()))
            .child(div().truncate().child(value.clone()))
            .into_any_element(),
        Row::SpreadName(name) => base()
            .text_xs()
            .text_color(look.muted)
            .child(name.clone())
            .into_any_element(),
        Row::Bar(value, share) => base()
            .text_xs()
            .child(div().w(px(160.)).truncate().child(value.clone()))
            .child(
                div().flex_1().h(px(6.)).rounded_sm().bg(look.folded).child(
                    div()
                        .h_full()
                        .w(gpui::relative(f32::from(*share) / 100.))
                        .rounded_sm()
                        .bg(look.primary),
                ),
            )
            .child(
                div()
                    .w(px(40.))
                    .text_right()
                    .text_color(look.muted)
                    .child(SharedString::from(format!("{share} %"))),
            )
            .into_any_element(),
        Row::Frame(at) => {
            let Some(frame) = frames.get(*at) else {
                return div().h(look.line).into_any_element();
            };
            let (app, path, line) = (
                app.clone(),
                std::path::PathBuf::from(&frame.path),
                frame.line,
            );
            base()
                .text_xs()
                .child(
                    Button::new(("sentry-frame", *at))
                        .ghost()
                        .xsmall()
                        .icon(icon(if frame.in_app { "file-code" } else { "file" }))
                        .label(SharedString::from(format!("{}:{}", frame.path, frame.line)))
                        // A frame Sentry names by a module, or one of a
                        // dependency that is not checked out here, opens
                        // nothing: the button says so by being dead rather than
                        // by opening an empty editor.
                        .disabled(!frame.opens)
                        .on_click(move |_, _window, cx| {
                            let path = path.clone();
                            app.update(cx, |app, cx| {
                                app.open_at(
                                    path,
                                    Some(crate::ui::explorer::Landing::Position {
                                        line: (line.saturating_sub(1)) as u32,
                                        character: 0,
                                    }),
                                    cx,
                                );
                            });
                        }),
                )
                .when(!frame.function.is_empty(), |el| {
                    el.child(
                        div()
                            .font_family(look.mono.clone())
                            .text_color(look.muted)
                            .truncate()
                            .child(SharedString::from(frame.function.clone())),
                    )
                })
                // **Ours, said out loud.** A stack runs a hundred frames deep
                // and three of them are the application's; the badge is what the
                // eye lands on, and it is the same three whose code is quoted
                // under them.
                .when(frame.in_app, |el| el.child(sentry_badge("app", look.info)))
                .into_any_element()
        }
        Row::Code(at, index) => {
            let Some((frame, (number, text))) = frames
                .get(*at)
                .and_then(|frame| Some((frame, frame.context.get(*index)?)))
            else {
                return div().h(look.line).into_any_element();
            };
            let culprit = *number == frame.line;
            let styles = frame.styles.get(*index).cloned().unwrap_or_default();
            let painted = match styles.is_empty() {
                false => gpui::StyledText::new(text.clone())
                    .with_highlights(styles)
                    .into_any_element(),
                true => div().child(text.clone()).into_any_element(),
            };
            base()
                .bg(if culprit { look.marked } else { look.ground })
                .font_family(look.mono.clone())
                .text_size(look.code)
                .child(
                    div()
                        .w(px(48.))
                        .flex_none()
                        .text_right()
                        .text_color(look.muted)
                        .child(SharedString::from(number.to_string())),
                )
                .child(div().whitespace_nowrap().child(painted))
                .into_any_element()
        }
    }
}

/// When something happened, as one reads it rather than as Sentry writes it.
///
/// `2026-08-29T00:15:45.042637Z` is a fact about a machine; what one wants of
/// "last seen" is how long ago, and of "first seen" is a date. Under a day the
/// answer is a duration — that is the reading that says whether it is still
/// happening — and past that it is the day itself, in the local timezone.
///
/// A text we cannot read is shown **as it stands**: it is Sentry's own, so a
/// format that changes is better read raw than guessed at.
fn when(text: &str) -> String {
    let Some(at) = crate::sentry::instant_of(text) else {
        return text.to_string();
    };
    let Some(instant) = chrono::DateTime::from_timestamp(at, 0) else {
        return text.to_string();
    };
    let minutes = chrono::Local::now()
        .signed_duration_since(instant)
        .num_minutes();
    match minutes {
        // Ahead of us is a clock out of step, and "in three minutes" said of an
        // error that has already happened reads as a bug in the page.
        i64::MIN..=0 => tr!("when-just-now").to_string(),
        1..=59 => tr!("when-minutes", { n: minutes }).to_string(),
        60..=1439 => tr!("when-hours", { n: minutes / 60 }).to_string(),
        _ => instant
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M")
            .to_string(),
    }
}

/// A word in a tinted pill: a level, a status, "app".
fn sentry_badge(text: &str, tone: gpui::Hsla) -> impl IntoElement {
    div()
        .px_1p5()
        .rounded_sm()
        .text_xs()
        .bg(tone.opacity(0.15))
        .text_color(tone)
        .child(SharedString::from(text.to_string()))
}

/// What a level is worth in colour. An unknown one is neutral rather than
/// alarming: Sentry's list is open, and a word we have never seen is not
/// necessarily bad news.
fn level_tone(level: &str, cx: &App) -> gpui::Hsla {
    match level {
        "fatal" | "error" => cx.theme().danger,
        "warning" => cx.theme().warning,
        "info" => cx.theme().info,
        _ => cx.theme().muted_foreground,
    }
}

/// And what a status is worth. Resolved is the one piece of good news this
/// page carries.
fn status_tone(status: &str, cx: &App) -> gpui::Hsla {
    match status {
        "resolved" => cx.theme().success,
        "ignored" => cx.theme().muted_foreground,
        _ => cx.theme().warning,
    }
}

/// A label and its value, read across.
fn sentry_pair(label: SharedString, value: String, cx: &App) -> impl IntoElement {
    h_flex()
        .gap_1()
        .text_xs()
        .child(div().text_color(cx.theme().muted_foreground).child(label))
        .child(div().child(SharedString::from(value)))
}
