//! Creating, starting, integrating and removing a worktree.
//!
//! Two sources meet here: git, which knows how to add a checkout and merge a
//! branch, and the project's `wt.toml`, which knows what has to be copied, which
//! ports to allocate and what to launch next.
//!
//! **The `wt.toml` is Claudhub's extension system.** A project's `[tasks.*]`
//! appear in a worktree's menu without Claudhub knowing what they do, its
//! `[[prompt]]`s become a dialog, its `[status] up` a badge in the sidebar. None
//! of that is compiled here: the project's file is what declares it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gpui::{div, prelude::*, px, App, Context, Entity, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputState},
    menu::{DropdownMenu, PopupMenuItem},
    v_flex, ActiveTheme, Sizable, StyledExt, WindowExt,
};

use crate::runtime::{Action, Cmd};
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::settings::Settings;
use crate::wt;

/// One round of a project's questions, as it comes back from the worker.
///
/// A struct rather than six parameters: it is one message, and it reads as one.
pub struct WtRound {
    pub main: PathBuf,
    pub slug: String,
    pub answers: BTreeMap<String, String>,
    pub questions: Vec<wt::Question>,
    pub round: u64,
}

/// Beyond this many options, a single choice goes back to being a menu: the
/// rows read better but they take the height of the dialog.
const ROWS_UP_TO: usize = 6;

/// Beyond this many options, a multiple choice gains a search field.
const FILTER_ABOVE: usize = 8;

/// A choice as one line of text: its label, then what the project says about
/// it. What the search matches on, and what a menu entry shows.
fn choice_line(choice: &wt::Choice) -> String {
    if choice.detail.is_empty() {
        choice.label.clone()
    } else {
        format!("{} — {}", choice.label, choice.detail)
    }
}

/// What the questions are being asked for.
///
/// The same three the `wt` command line knows, and there is no fourth: a
/// gesture that asks nothing does not go through the dialog at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WtTarget {
    /// `wt new`: nothing is checked out yet, so there is no worktree to name.
    Create {
        /// The branch's starting point, when starting from an existing branch.
        from: Option<String>,
    },
    /// `wt up`: the answers become the `--set`s of the start.
    Up { worktree: PathBuf },
    /// A task that declares prompts of its own: the answers become its
    /// arguments.
    Task { worktree: PathBuf, task: String },
}

impl WtTarget {
    fn phase(&self) -> wt::Phase {
        match self {
            Self::Create { .. } => wt::Phase::New,
            Self::Up { .. } => wt::Phase::Up,
            Self::Task { .. } => wt::Phase::Task,
        }
    }

    fn task(&self) -> Option<String> {
        match self {
            Self::Task { task, .. } => Some(task.clone()),
            _ => None,
        }
    }
}

/// A project's questions, between the gesture and the command it ends in.
///
/// It lives on the application and not in the dialog: the questions arrive
/// through an event — a `[[prompt]]` with `source` launches a shell — and a
/// dialog closure has nowhere to file them.
pub struct WtPrompt {
    pub main: PathBuf,
    pub slug: String,
    pub target: WtTarget,
    pub questions: Vec<wt::Question>,
    pub answers: BTreeMap<String, String>,
    /// The free fields, created when the questions arrive and never in a render:
    /// recreated per frame, they would lose the cursor on the first keystroke.
    pub inputs: BTreeMap<String, Entity<InputState>>,
    /// The search field of a long multiple choice, same rule and same reason.
    pub filters: BTreeMap<String, Entity<InputState>>,
    /// A request for questions has gone out and not come back.
    pub asking: bool,
    /// The round in flight. A counter and not a comparison of the answers: the
    /// worker seeds them for a `wt up`, so what comes back is not what went out.
    pub round: u64,
}

impl ClaudhubApp {
    // — What the project declares ———————————————————————————————

    /// A repository's `wt.toml`, if it has one and we have already read it.
    pub(super) fn wt_project(&self, main: &Path) -> Option<&wt::Snapshot> {
        self.wt_projects.get(main)?.as_ref()
    }

    /// Asks for a repository's `wt.toml`, once.
    pub(super) fn ensure_wt_project(&mut self, main: &Path) {
        if self.wt_projects.contains_key(main) {
            return;
        }
        // Marked as read at once: without that, every frame of the menu would
        // restart the command for the whole length of the read.
        self.wt_projects.insert(main.to_path_buf(), None);
        self.git.send(Cmd::WtLoad {
            main: main.to_path_buf(),
        });
    }

    pub(super) fn wt_state(
        &self,
        worktree: &Path,
    ) -> Option<&crate::runtime::protocol::WtWorktree> {
        self.wt_states.get(worktree)
    }

    /// The background reading: state and addresses of every worktree `wt` manages.
    pub(super) fn scan_wt(&mut self) {
        let targets: Vec<(PathBuf, PathBuf)> = self
            .repos
            .iter()
            .filter(|repo| self.wt_project(&repo.main).is_some())
            .flat_map(|repo| {
                repo.worktrees
                    .iter()
                    .map(|w| (repo.main.clone(), w.path.clone()))
            })
            .collect();
        if targets.is_empty() {
            return;
        }
        self.git.send(Cmd::WtScan { targets });
    }

    // — Create ————————————————————————————————————————————————

    /// Starts the guided creation of a worktree.
    ///
    /// Without a `wt.toml`, we fall back to the bare git add: a repository with
    /// no configuration must still be able to gain a worktree.
    pub(super) fn start_worktree(
        &mut self,
        main: PathBuf,
        slug: String,
        from: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let slug = slug.trim().to_string();
        if slug.is_empty() {
            return;
        }
        let Some(project) = self.wt_project(&main).cloned() else {
            self.add_worktree_without_wt(&main, &slug, from.as_deref(), cx);
            return;
        };
        if !project.has_new_prompts {
            self.git.send(Cmd::WtCreate {
                main,
                slug,
                from,
                answers: BTreeMap::new(),
            });
            cx.notify();
            return;
        }
        self.ask_wt(main, slug, WtTarget::Create { from }, window, cx);
    }

    /// Opens the question dialog and asks for the first round.
    ///
    /// The dialog opens **at once**, with its waiting note: the questions go
    /// through a shell — the Acetics tenants are a MariaDB query — and a window
    /// that does not react for a second reads as a lost click.
    fn ask_wt(
        &mut self,
        main: PathBuf,
        slug: String,
        target: WtTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (phase, task) = (target.phase(), target.task());
        self.creation = Some(WtPrompt {
            main: main.clone(),
            slug: slug.clone(),
            target,
            questions: Vec::new(),
            answers: BTreeMap::new(),
            inputs: BTreeMap::new(),
            filters: BTreeMap::new(),
            asking: true,
            round: 0,
        });
        self.git.send(Cmd::WtQuestions {
            main,
            slug,
            answers: BTreeMap::new(),
            phase,
            task,
            round: 0,
        });
        self.open_creation_dialog(window, cx);
        cx.notify();
    }

    /// The bare git add, for a repository with no `wt.toml`.
    ///
    /// The worktree is created beside the repository, in `<repo>-wt/<name>`:
    /// that is `wt`'s convention, so both tools see the same folders the day the
    /// project gains a configuration.
    fn add_worktree_without_wt(
        &mut self,
        main: &Path,
        slug: &str,
        from: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let repo_name = main
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repo".into());
        let root = main
            .parent()
            .map(|p| p.join(format!("{repo_name}-wt")))
            .unwrap_or_else(|| PathBuf::from(format!("{repo_name}-wt")));
        self.git.send(Cmd::AddWorktree {
            main: main.to_path_buf(),
            path: root.join(slug),
            branch: format!("wt/{slug}"),
            from: from.map(str::to_string),
        });
        cx.notify();
    }

    /// Receives the project's questions and prepares their fields.
    pub(super) fn wt_questions_arrived(
        &mut self,
        round: WtRound,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let WtRound {
            main,
            slug,
            answers,
            questions,
            round,
        } = round;
        let Some(creation) = self.creation.as_mut() else {
            return;
        };
        // A late answer, for another worktree or an older round, would replace
        // the questions with the wrong ones.
        if creation.main != main || creation.slug != slug || creation.round != round {
            return;
        }
        creation.asking = false;
        // The worker may have seeded them — a `wt up` starts from what the
        // worktree remembers — so what comes back is what counts.
        creation.answers = answers;
        // Nothing left to ask: the project has finished asking its questions.
        if questions.is_empty() {
            self.run_wt_target(window, cx);
            return;
        }
        // The default values are set at once: a single-choice question whose
        // menu is left untouched has to go out with its suggested value, not
        // with nothing.
        let mut inputs = BTreeMap::new();
        let mut filters = BTreeMap::new();
        for question in &questions {
            let value = question.default.clone().unwrap_or_else(|| {
                match question.kind {
                    // A choice with no default takes the first: that is what the
                    // menu shows, and leaving it empty would make the display
                    // lie.
                    wt::Kind::Choice => question
                        .choices
                        .first()
                        .map(|c| c.value.clone())
                        .unwrap_or_default(),
                    wt::Kind::Confirm => "0".into(),
                    _ => String::new(),
                }
            });
            if matches!(question.kind, wt::Kind::Text) {
                let start = value.clone();
                let input = cx.new(|cx| InputState::new(window, cx).default_value(start));
                inputs.insert(question.name.clone(), input);
            }
            if matches!(question.kind, wt::Kind::Multi) && question.choices.len() > FILTER_ABOVE {
                let input = cx.new(|cx| {
                    InputState::new(window, cx).placeholder(tr!("worktree-filter-choices"))
                });
                filters.insert(question.name.clone(), input);
            }
            creation.answers.insert(question.name.clone(), value);
        }
        creation.questions = questions;
        creation.inputs = inputs;
        creation.filters = filters;
        cx.notify();
    }

    /// Records an answer and asks the questions again: a `when` may unlock
    /// another.
    fn answer(&mut self, name: String, value: String, cx: &mut Context<Self>) {
        let Some(creation) = self.creation.as_mut() else {
            return;
        };
        creation.answers.insert(name, value);
        cx.notify();
    }

    /// The project has nothing left to ask: run what the questions were for.
    fn run_wt_target(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.creation.take() else {
            return;
        };
        window.close_all_dialogs(cx);
        let WtPrompt {
            main,
            slug,
            target,
            answers,
            ..
        } = prompt;
        match target {
            WtTarget::Create { from } => {
                self.start(
                    None,
                    Action::Worktree,
                    Cmd::WtCreate {
                        main,
                        slug,
                        from,
                        answers,
                    },
                    cx,
                );
            }
            WtTarget::Up { worktree } => {
                self.flight.set_wt_target(worktree);
                self.start(
                    None,
                    Action::WtUp,
                    Cmd::WtUp {
                        main,
                        slug,
                        answers,
                    },
                    cx,
                );
            }
            WtTarget::Task { worktree, task } => {
                self.git.send(Cmd::WtTask {
                    main,
                    worktree,
                    slug,
                    task,
                    answers,
                });
                cx.notify();
            }
        }
    }

    /// Confirms the current page and asks for the next.
    fn submit_answers(&mut self, cx: &mut Context<Self>) {
        let Some(creation) = self.creation.as_mut() else {
            return;
        };
        // The free fields are only read here: listening to them as they are
        // typed would restart a shell per character, every question having a
        // `when` that may depend on them.
        let typed: Vec<(String, String)> = creation
            .inputs
            .iter()
            .map(|(name, input)| (name.clone(), input.read(cx).value().to_string()))
            .collect();
        for (name, value) in typed {
            creation.answers.insert(name, value);
        }
        creation.asking = true;
        creation.round += 1;
        let (main, slug, answers, round) = (
            creation.main.clone(),
            creation.slug.clone(),
            creation.answers.clone(),
            creation.round,
        );
        let (phase, task) = (creation.target.phase(), creation.target.task());
        self.git.send(Cmd::WtQuestions {
            main,
            slug,
            answers,
            phase,
            task,
            round,
        });
        cx.notify();
    }

    /// The creation dialog.
    ///
    /// It redraws on every frame from `Creation`: the questions arrive after it
    /// opens, and content frozen at construction would stay empty.
    fn open_creation_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entity = cx.entity();
        // The title says which gesture the questions belong to: the same dialog
        // now serves three, and "New worktree" above the tenants of a `wt up`
        // would be a plain lie.
        let title = match self.creation.as_ref().map(|prompt| &prompt.target) {
            Some(WtTarget::Up { .. }) => tr!("worktree-up-title"),
            Some(WtTarget::Task { task, .. }) => SharedString::from(task.clone()),
            _ => tr!("worktree-new-title"),
        };
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let entity = entity.clone();
            let cancel = entity.clone();
            dialog
                .title(title.clone())
                .child(
                    div()
                        .w(px(520.))
                        .child(entity.clone().update(_cx, |this, cx| {
                            this.render_creation_body(cx).into_any_element()
                        })),
                )
                .overlay_closable(false)
                .close_button(false)
                .footer(super::dialogs::confirm())
                .on_ok(move |_, _window, cx| {
                    entity.update(cx, |this, cx| this.submit_answers(cx));
                    // The dialog stays open: the next page shows in it, and it is
                    // the absence of a new question that closes it.
                    false
                })
                .on_cancel(move |_, _window, cx| {
                    cancel.update(cx, |this, _| this.creation = None);
                    true
                })
        });
    }

    fn render_creation_body(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(creation) = self.creation.as_ref() else {
            return div().into_any_element();
        };
        if creation.asking {
            return div()
                .p_4()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(tr!("worktree-asking"))
                .into_any_element();
        }
        let questions = creation.questions.clone();
        let answers = creation.answers.clone();
        let inputs = creation.inputs.clone();
        let muted = cx.theme().muted_foreground;

        let mut rows = Vec::new();
        for question in questions {
            let current = answers.get(&question.name).cloned().unwrap_or_default();
            let field = match question.kind {
                wt::Kind::Text => inputs
                    .get(&question.name)
                    .map(|input| Input::new(input).small().into_any_element())
                    .unwrap_or_else(|| div().into_any_element()),
                wt::Kind::Confirm => {
                    Checkbox::new(SharedString::from(format!("wt-confirm-{}", question.name)))
                        .checked(current == "1")
                        .on_click({
                            let (entity, name, was) =
                                (cx.entity(), question.name.clone(), current == "1");
                            move |_, _window, cx| {
                                let value = if was { "0" } else { "1" };
                                entity.update(cx, |this, cx| {
                                    this.answer(name.clone(), value.into(), cx)
                                });
                            }
                        })
                        .into_any_element()
                }
                wt::Kind::Choice => self
                    .render_choice(&question, &current, cx)
                    .into_any_element(),
                wt::Kind::Multi => self
                    .render_multi(&question, &current, cx)
                    .into_any_element(),
            };
            rows.push(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().child(question.title.clone()))
                    .child(field)
                    .into_any_element(),
            );
        }

        v_flex()
            .gap_3()
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(SharedString::from(creation.slug.clone())),
            )
            .children(rows)
            .into_any_element()
    }

    /// A single choice.
    ///
    /// **Rows and not a dropdown while the list is short.** Each option carries
    /// a `detail` — "obligatoire si la branche contient une migration" — which
    /// is the sentence the choice is actually made on, and a dropdown hides it
    /// behind a click that also commits the answer. Past a handful of options
    /// the rows would take the whole dialog, and the menu comes back.
    fn render_choice(
        &self,
        question: &wt::Question,
        current: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if question.choices.len() > ROWS_UP_TO {
            return self
                .render_choice_menu(question, current, cx)
                .into_any_element();
        }
        let muted = cx.theme().muted_foreground;
        let rows = question.choices.iter().enumerate().map(|(index, choice)| {
            let selected = choice.value == current;
            let entity = cx.entity();
            let (name, value) = (question.name.clone(), choice.value.clone());
            h_flex()
                .id(SharedString::from(format!(
                    "wt-choice-{}-{index}",
                    question.name
                )))
                .w_full()
                .px_2()
                .py_1()
                .gap_2()
                .items_baseline()
                .rounded(cx.theme().radius)
                .cursor_pointer()
                .when(selected, |el| el.bg(cx.theme().accent))
                .hover(|s| s.bg(cx.theme().accent.opacity(0.5)))
                .on_click(move |_, _window, cx| {
                    entity.update(cx, |this, cx| this.answer(name.clone(), value.clone(), cx));
                })
                .child(
                    div()
                        .flex_none()
                        .text_sm()
                        .when(selected, |el| el.font_semibold())
                        .child(SharedString::from(choice.label.clone())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(muted)
                        .child(SharedString::from(choice.detail.clone())),
                )
        });
        v_flex().gap_px().children(rows).into_any_element()
    }

    /// The same, as a menu, when there are too many options for rows.
    fn render_choice_menu(
        &self,
        question: &wt::Question,
        current: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let label = question
            .choices
            .iter()
            .find(|choice| choice.value == current)
            .map(|choice| choice.label.clone())
            .unwrap_or_else(|| current.to_string());
        let entity = cx.entity();
        let (name, choices) = (question.name.clone(), question.choices.clone());
        Button::new(SharedString::from(format!("wt-choice-{}", question.name)))
            .outline()
            .small()
            .label(SharedString::from(label))
            .dropdown_menu(move |menu, _window, _cx| {
                choices.iter().fold(menu, |menu, choice| {
                    let (entity, name, value) =
                        (entity.clone(), name.clone(), choice.value.clone());
                    menu.item(
                        PopupMenuItem::new(SharedString::from(choice_line(choice))).on_click(
                            move |_, _window, cx| {
                                entity.update(cx, |this, cx| {
                                    this.answer(name.clone(), value.clone(), cx)
                                });
                            },
                        ),
                    )
                })
            })
    }

    /// A multiple choice: one box per value, joined by the separator the project
    /// declares.
    ///
    /// **A search field as soon as the list is long, and a bounded height.** The
    /// options often come from a shell command — on Acetics the tenants are a
    /// MariaDB query, and there are eighty of them — and eighty checkboxes with
    /// no way to narrow them is a list one scrolls past instead of reading. It
    /// is what `wt`'s own interface does with skim, and it is the reason the
    /// question exists at all.
    ///
    /// The filter hides rows; it never touches the answer. A tenant chosen then
    /// filtered out stays chosen — the count says so — because a search that
    /// silently unselects is the one way to clone the wrong databases.
    fn render_multi(
        &self,
        question: &wt::Question,
        current: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let separator = question.separator.clone();
        let chosen: Vec<String> = current
            .split(separator.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect();
        let filter = self
            .creation
            .as_ref()
            .and_then(|prompt| prompt.filters.get(&question.name));
        let needle = filter
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_default();
        let muted = cx.theme().muted_foreground;

        let mut boxes = Vec::new();
        for (index, choice) in question.choices.iter().enumerate() {
            if !crate::ui::find::matches(&needle, &choice_line(choice)) {
                continue;
            }
            let checked = chosen.iter().any(|value| value == &choice.value);
            let entity = cx.entity();
            let (name, value, separator) = (
                question.name.clone(),
                choice.value.clone(),
                separator.clone(),
            );
            let chosen = chosen.clone();
            boxes.push(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_baseline()
                    .child(
                        Checkbox::new(SharedString::from(format!(
                            "wt-multi-{}-{index}",
                            question.name
                        )))
                        .label(SharedString::from(choice.label.clone()))
                        .checked(checked)
                        .on_click(move |_, _window, cx| {
                            let mut next = chosen.clone();
                            if checked {
                                next.retain(|v| v != &value);
                            } else {
                                next.push(value.clone());
                            }
                            let joined = next.join(&separator);
                            entity.update(cx, |this, cx| this.answer(name.clone(), joined, cx));
                        }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(muted)
                            .child(SharedString::from(choice.detail.clone())),
                    )
                    .into_any_element(),
            );
        }

        v_flex()
            .gap_1()
            .when_some(filter.cloned(), |el, input| {
                el.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(div().flex_1().child(Input::new(&input).small()))
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(muted)
                                .child(tr!("worktree-chosen-count", { count: chosen.len() })),
                        ),
                )
            })
            // Bounded and scrolling: eighty tenants would push the dialog's
            // buttons off the bottom of the window, where nothing would reach
            // them.
            .child(
                div()
                    .id(SharedString::from(format!("wt-multi-{}", question.name)))
                    .max_h(px(240.))
                    .overflow_y_scroll()
                    .child(v_flex().gap_1().children(boxes)),
            )
    }

    // — Start, remove, run ————————————————————————————————————

    /// Starts a worktree, asking the project's `ask = "up"` questions first.
    ///
    /// That is what the command line does, and what was missing here: on
    /// Acetics, the tenants whose storage gets mounted and the services of the
    /// container are chosen at **every** start until they have been chosen once
    /// — and starting without them mounts nothing, in silence. A project that
    /// asks nothing goes straight through, as before.
    pub(super) fn wt_up(
        &mut self,
        main: PathBuf,
        worktree: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(slug) = self.wt_slug(&main, worktree) else {
            return;
        };
        if self
            .wt_project(&main)
            .is_some_and(|project| project.has_up_prompts)
        {
            let target = WtTarget::Up {
                worktree: worktree.to_path_buf(),
            };
            self.ask_wt(main, slug, target, window, cx);
            return;
        }
        // Starting a project brings up containers and waits for them: tens of
        // seconds during which the only thing that says anything is happening is
        // this dot.
        self.flight.set_wt_target(worktree.to_path_buf());
        let cmd = Cmd::WtUp {
            main,
            slug,
            answers: BTreeMap::new(),
        };
        self.start(None, Action::WtUp, cmd, cx);
    }

    pub(super) fn wt_down(&mut self, main: PathBuf, worktree: &Path, cx: &mut Context<Self>) {
        let Some(slug) = self.wt_slug(&main, worktree) else {
            return;
        };
        self.flight.set_wt_target(worktree.to_path_buf());
        self.start(None, Action::WtDown, Cmd::WtDown { main, slug }, cx);
    }

    pub(super) fn wt_remove(&mut self, main: PathBuf, worktree: &Path, cx: &mut Context<Self>) {
        let Some(slug) = self.wt_slug(&main, worktree) else {
            return;
        };
        // `worktrees_changed` answers without naming a checkout — the one that
        // was removed no longer exists — so the key is the bare action.
        self.start(None, Action::Worktree, Cmd::WtRemove { main, slug }, cx);
    }

    /// Launches a project task in a terminal tab, asking its own questions
    /// first.
    ///
    /// A task may declare `prompt = [...]`: the answers become its arguments —
    /// `db-add` picks the tenants to clone that way. Without them the task ran
    /// with no argument at all, which on a task declared `interactive` means its
    /// own picker in the terminal, and on the others means nothing chosen.
    pub(super) fn wt_task(
        &mut self,
        main: PathBuf,
        worktree: PathBuf,
        task: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(slug) = self.wt_slug(&main, &worktree) else {
            return;
        };
        let asks = self
            .wt_project(&main)
            .into_iter()
            .flat_map(|project| project.tasks.iter())
            .any(|info| info.name == task && info.prompts);
        if asks {
            let target = WtTarget::Task { worktree, task };
            self.ask_wt(main, slug, target, window, cx);
            return;
        }
        self.git.send(Cmd::WtTask {
            main,
            worktree,
            slug,
            task,
            answers: BTreeMap::new(),
        });
        cx.notify();
    }

    /// Receives a ready task and opens it in a terminal.
    pub(super) fn wt_task_ready(
        &mut self,
        worktree: PathBuf,
        task: String,
        launch: wt::Launch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let line = launch.shell_line();
        if line.trim().is_empty() {
            return;
        }
        self.show_terminal_panel(window, cx);
        self.open_terminal(
            &worktree,
            crate::ui::terminal_view::Launch {
                // A shell and not the bare program: a task is one of the
                // project's command lines, with its pipes and its
                // redirections, and it is a shell that can read them.
                command: Some(("sh".into(), vec!["-lc".into(), line])),
                env: launch.env.into_iter().collect(),
                label: SharedString::from(task),
                agent: false,
            },
            window,
            cx,
        );
    }

    /// A worktree's slug, when `wt` knows it.
    fn wt_slug(&self, main: &Path, worktree: &Path) -> Option<String> {
        let root = &self.wt_project(main)?.root;
        let rest = worktree.strip_prefix(root).ok()?;
        let mut parts = rest.components();
        let slug = parts.next()?.as_os_str().to_str()?.to_string();
        parts.next().is_none().then_some(slug)
    }

    // — Integrate ——————————————————————————————————————————————

    /// Brings the worktree up to date from its base: the base has moved on while
    /// the agent was working.
    pub(super) fn update_from_base(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(base) = self.active_review().and_then(|state| state.base.clone()) else {
            return;
        };
        let rebase = Settings::global(cx).update_with_rebase;
        let (action, cmd) = if rebase {
            (
                Action::Rebase,
                Cmd::Rebase {
                    worktree: worktree.clone(),
                    onto: base,
                },
            )
        } else {
            (
                Action::Merge,
                Cmd::Merge {
                    worktree: worktree.clone(),
                    from: base,
                    no_ff: false,
                },
            )
        };
        self.start(Some(worktree), action, cmd, cx);
    }

    /// Integrates a worktree's branch into its base, from the main repository —
    /// the only place the base is updated from.
    pub(super) fn integrate(&mut self, worktree: PathBuf, cx: &mut Context<Self>) {
        let Some(main) = self.main_of(&worktree) else {
            return;
        };
        let Some(branch) = self
            .repos
            .iter()
            .flat_map(|repo| repo.worktrees.iter())
            .find(|w| w.path == worktree)
            .and_then(|w| w.branch.clone())
        else {
            return;
        };
        let Some(base) = self
            .review
            .get(&worktree)
            .and_then(|state| state.base.clone())
        else {
            return;
        };
        // The base may be a remote branch (`origin/main`); it is the main
        // repository's that gets updated, so its short name.
        let base = base
            .rsplit_once('/')
            .map(|(_, b)| b)
            .unwrap_or(&base)
            .to_string();
        // Recorded before the send: it is on the success arriving that the
        // cleanup is offered, and the worktree cannot be derived from the answer.
        self.integrated = Some((worktree, branch.clone()));
        // The worker runs it **from the main repository**, and that is the
        // worktree the answer names: an integration is the base's business, not
        // the branch's.
        let cmd = Cmd::Integrate {
            main: main.clone(),
            branch,
            base,
            no_ff: Settings::global(cx).integrate_no_ff,
        };
        self.start(Some(main), Action::Integrate, cmd, cx);
    }

    /// An integration has succeeded: offer to remove the worktree and its
    /// branch.
    ///
    /// The question is asked because `wt` deliberately keeps the branch: what is
    /// left after a `wt rm` is a choice, and it is Claudhub's job to ask rather
    /// than `wt`'s to assume.
    pub(super) fn offer_cleanup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((worktree, branch)) = self.integrated.take() else {
            return;
        };
        let Some(main) = self.main_of(&worktree) else {
            return;
        };
        let label = SharedString::from(format!("{} · {branch}", worktree.display()));
        let entity = cx.entity();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (worktree, branch, main, entity) = (
                worktree.clone(),
                branch.clone(),
                main.clone(),
                entity.clone(),
            );
            dialog
                .title(tr!("worktree-cleanup-title"))
                .child(
                    v_flex()
                        .gap_1()
                        .child(div().text_sm().child(label.clone()))
                        .child(div().text_xs().child(tr!("worktree-cleanup-help"))),
                )
                .overlay_closable(false)
                .close_button(false)
                .footer(super::dialogs::confirm())
                .on_ok(move |_, _window, cx| {
                    entity.update(cx, |this, cx| {
                        this.wt_remove(main.clone(), &worktree, cx);
                        this.git.send(Cmd::DeleteBranch {
                            main: main.clone(),
                            name: branch.clone(),
                            force: false,
                        });
                    });
                    true
                })
        });
    }

    /// A worktree's context menu: git on one side, the project on the other.
    pub(super) fn worktree_menu(
        &mut self,
        menu: gpui_component::menu::PopupMenu,
        main: PathBuf,
        worktree: PathBuf,
        cx: &mut Context<Self>,
    ) -> gpui_component::menu::PopupMenu {
        self.ensure_wt_project(&main);
        let entity = cx.entity();
        let project = self.wt_project(&main).cloned();
        let known = self.wt_slug(&main, &worktree).is_some();

        let mut menu = {
            let (update, integrate) = (entity.clone(), entity.clone());
            let (for_update, for_integrate) = (worktree.clone(), worktree.clone());
            menu.item(
                PopupMenuItem::new(tr!("worktree-update"))
                    .icon(icon("arrow-down-to-line"))
                    .on_click(move |_, window, cx| {
                        update.update(cx, |this, cx| {
                            this.select_worktree(for_update.clone(), window, cx);
                            this.update_from_base(cx);
                        });
                    }),
            )
            .item(
                PopupMenuItem::new(tr!("worktree-integrate"))
                    .icon(icon("git-merge"))
                    .on_click(move |_, _window, cx| {
                        integrate.update(cx, |this, cx| this.integrate(for_integrate.clone(), cx));
                    }),
            )
        };

        // Removing the checkout itself, when `wt` is not the one keeping the
        // books: it was the sidebar row's dustbin, and the sidebar is gone. Not
        // offered on the main worktree — git refuses, and the entry would
        // promise an error.
        let Some(project) = project.filter(|_| known) else {
            if worktree != main {
                let (entity, main, path) = (entity.clone(), main.clone(), worktree.clone());
                menu = menu.separator().item(
                    PopupMenuItem::new(tr!("worktree-remove-checkout"))
                        .icon(icon("trash-2"))
                        .on_click(move |_, window, cx| {
                            entity.update(cx, |this, cx| {
                                this.confirm_remove_worktree(main.clone(), path.clone(), window, cx)
                            });
                        }),
                );
            }
            return Self::forget_entry(menu, entity, main);
        };

        menu = menu.separator();
        if project.has_up {
            let (entity, main, worktree) = (entity.clone(), main.clone(), worktree.clone());
            menu = menu.item(
                PopupMenuItem::new(tr!("worktree-up"))
                    .icon(icon("play"))
                    .on_click(move |_, window, cx| {
                        entity.update(cx, |this, cx| {
                            this.wt_up(main.clone(), &worktree, window, cx)
                        });
                    }),
            );
        }
        if project.has_down {
            let (entity, main, worktree) = (entity.clone(), main.clone(), worktree.clone());
            menu = menu.item(
                PopupMenuItem::new(tr!("worktree-down"))
                    .icon(icon("circle-stop"))
                    .on_click(move |_, _window, cx| {
                        entity.update(cx, |this, cx| this.wt_down(main.clone(), &worktree, cx));
                    }),
            );
        }
        {
            let (entity, main, worktree) = (entity.clone(), main.clone(), worktree.clone());
            menu = menu.item(
                PopupMenuItem::new(tr!("worktree-remove"))
                    .icon(icon("trash-2"))
                    .on_click(move |_, _window, cx| {
                        entity.update(cx, |this, cx| this.wt_remove(main.clone(), &worktree, cx));
                    }),
            );
        }

        // The project's tasks, as it declares them. Claudhub does not know what
        // they do, and that is the point.
        if project.tasks.is_empty() {
            return Self::forget_entry(menu, entity, main);
        }
        menu = menu.separator();
        let menu = project.tasks.into_iter().fold(menu, |menu, task| {
            let (entity, main, worktree) = (entity.clone(), main.clone(), worktree.clone());
            let name = task.name.clone();
            let label = if task.description.is_empty() {
                task.name.clone()
            } else {
                format!("{} — {}", task.name, task.description)
            };
            menu.item(
                PopupMenuItem::new(SharedString::from(label))
                    .icon(icon("terminal"))
                    .on_click(move |_, window, cx| {
                        entity.update(cx, |this, cx| {
                            this.wt_task(main.clone(), worktree.clone(), name.clone(), window, cx)
                        });
                    }),
            )
        });
        Self::forget_entry(menu, entity, main)
    }

    /// Closing the repository, at the bottom of every worktree menu.
    ///
    /// It was the sidebar's right click on a repository heading, and it has to keep
    /// living somewhere: nothing on disk is touched, but everything opened in the
    /// repository is closed, which is not a gesture one makes twice a day — hence
    /// its place, last and behind a separator, rather than a button one brushes
    /// past.
    fn forget_entry(
        menu: gpui_component::menu::PopupMenu,
        entity: Entity<ClaudhubApp>,
        main: PathBuf,
    ) -> gpui_component::menu::PopupMenu {
        menu.separator().item(
            PopupMenuItem::new(tr!("repo-forget"))
                .icon(icon("x"))
                .on_click(move |_, window, cx| {
                    entity.update(cx, |this, cx| {
                        this.forget_repository(main.clone(), window, cx)
                    });
                }),
        )
    }

    /// A worktree's "open" button, when the project exposes an address.
    pub(super) fn render_wt_links(
        &self,
        worktree: &Path,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let endpoints = self.wt_state(worktree)?.endpoints.clone();
        let first = endpoints.first()?.clone();
        Some(
            Button::new(SharedString::from(format!(
                "wt-open-{}",
                worktree.display()
            )))
            .ghost()
            .xsmall()
            .icon(icon("external-link"))
            .tooltip(SharedString::from(first.label.clone()))
            .on_click(cx.listener(move |_, _, _window, cx| {
                cx.open_url(&first.url);
            })),
        )
    }

    /// The state badge of a worktree `wt` knows how to start.
    pub(super) fn render_wt_state(
        &self,
        worktree: &Path,
        cx: &gpui::App,
    ) -> Option<impl IntoElement> {
        let up = self.wt_state(worktree)?.up?;
        // Starting or stopping is the only state the dot cannot show, being
        // neither of the two it knows: it becomes a spinner, and the row is the
        // only place that says anything for the next half-minute.
        if self.flight.wt_target() == Some(worktree) {
            return Some(
                h_flex()
                    .flex_none()
                    .child(
                        icon("loader-circle")
                            .xsmall()
                            .text_color(cx.theme().warning),
                    )
                    .into_any_element(),
            );
        }
        let color = if up {
            cx.theme().success
        } else {
            cx.theme().muted_foreground
        };
        Some(
            h_flex()
                .flex_none()
                .child(
                    div()
                        .size(px(7.))
                        .rounded_full()
                        .when(up, |el| el.bg(color))
                        .when(!up, |el| el.border_1().border_color(color.opacity(0.8))),
                )
                .into_any_element(),
        )
    }

    /// Removes a repository from the list. Nothing on disk is touched.
    ///
    /// The same gesture for an open repository and for an unreachable one: in
    /// both cases what is removed is an entry from the list of repositories
    /// reopened at startup, and the second case is the only one where it could
    /// not be done — hence the report.
    pub(super) fn forget_repository(
        &mut self,
        main: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::ui::settings::Settings::update_global(cx, |s| s.forget_repository(&main));
        let closed = self.repos.close(&main);
        // What we kept of its worktrees has no further purpose. The state store,
        // for its part, is not purged: notes and collapses wait for the day the
        // repository is reopened, and erasing them here would turn a tidy-up
        // into a loss.
        for worktree in &closed {
            self.review.remove(worktree);
            self.close_terminals_of(worktree, window, cx);
            self.summaries.remove(worktree);
        }
        if self
            .active
            .as_deref()
            .is_some_and(|active| closed.iter().any(|path| path == active))
        {
            self.active = None;
            if let Some(first) = self.first_worktree() {
                self.select_worktree(first, window, cx);
            }
        }
        cx.notify();
    }

    /// Opens a repository. The native folder picker is asynchronous: the answer
    /// comes back in a task, hence the `spawn`.
    pub(super) fn prompt_open_repository(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return; // cancelled
            };
            let _ = this.update(cx, |this, cx| {
                for path in paths {
                    match this.repo_path_for_server(path, cx) {
                        Ok(path) => this.git.send(Cmd::OpenRepo(path)),
                        Err(message) => this.announce(message, cx),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The path the native picker returned, as the server will understand it.
    ///
    /// On Windows the dialog returns `\\wsl.localhost\Ubuntu\home\…` or `C:\…`;
    /// the wire, for its part, carries only Linux paths — the server's disk is
    /// authoritative. It is one of the few points where a path **enters** from
    /// the Windows world, and therefore one of the few where it has to be
    /// translated. Elsewhere there is nothing to do.
    fn repo_path_for_server(&self, path: PathBuf, cx: &App) -> Result<PathBuf, SharedString> {
        if !cfg!(windows) {
            return Ok(path);
        }
        let distro = Settings::global(cx).wsl_distro.clone();
        let Some(translated) = crate::wslpath::to_linux(&path) else {
            return Err(tr!("repo-not-in-wsl"));
        };
        // A repository from a **different** distribution than the server's: its
        // path is valid over there and unreachable here, and opening it would
        // give an empty folder without saying why.
        if let Some(other) = translated
            .distro
            .filter(|d| !d.eq_ignore_ascii_case(&distro))
        {
            return Err(tr!("repo-other-distro", { distro: other }));
        }
        Ok(translated.path)
    }

    pub(super) fn prompt_new_worktree(
        &mut self,
        main: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_text_dialog(
            tr!("worktree-new-title"),
            tr!("worktree-new-placeholder"),
            window,
            cx,
            // The name typed in, then what the project asks for: its
            // `[[prompt]]`s become a dialog, its copies and its ports are done
            // by `wt`. Without a `wt.toml`, the bare git add is enough — a
            // repository with no configuration must still be able to gain a
            // worktree.
            move |this, name, window, cx| {
                this.start_worktree(main.clone(), name, None, window, cx);
            },
        );
    }

    pub(super) fn confirm_remove_worktree(
        &mut self,
        main: PathBuf,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let label = path.display().to_string();
        let entity = cx.entity();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (main, path, entity) = (main.clone(), path.clone(), entity.clone());
            dialog
                .title(tr!("worktree-remove-title"))
                .child(div().text_sm().child(label.clone()))
                .overlay_closable(false)
                .close_button(false)
                .footer(super::dialogs::confirm())
                .on_ok(move |_, _window, cx| {
                    entity.update(cx, |this, cx| {
                        this.git.send(Cmd::RemoveWorktree {
                            main: main.clone(),
                            path: path.clone(),
                            // Without `force`, git refuses to remove a worktree
                            // that has changes — that is the protection we want
                            // here, and the message will say so.
                            force: false,
                        });
                        cx.notify();
                    });
                    true
                })
        });
    }
}
