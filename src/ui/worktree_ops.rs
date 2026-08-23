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

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{div, prelude::*, px, App, Context, Entity, Render, SharedString, Window};
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
        /// An existing branch to check out; `None` is a new branch, named by
        /// the project's template.
        branch: Option<String>,
        /// Where a new branch starts; `None` is the main repository's HEAD.
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

/// The first page of a creation: which branch, where it starts, what folder.
///
/// It is the page `wt`'s own interface shows as three — branch, base, slug —
/// and the same three decisions, in the same order. The branch list is the one
/// the top bar's picker shows, read from the repository on every frame: the
/// page opens before the branches have been re-read, and a list frozen at
/// construction would miss the one just fetched.
pub struct Setup {
    /// The branch chosen; `None` is "a new branch".
    pub branch: Option<String>,
    /// Where a new branch starts; `None` is the main repository's HEAD.
    pub from: Option<String>,
    /// The folder name. A field and not a string, for the reason every field of
    /// this dialog is one: recreated per frame, it would lose the caret on the
    /// first keystroke.
    pub slug: Entity<InputState>,
    /// The search fields of the two branch lists, shown past [`FILTER_ABOVE`].
    pub branch_filter: Entity<InputState>,
    pub base_filter: Entity<InputState>,
    /// Why the page would not submit, said under the field rather than in a
    /// balloon the next frame hides.
    pub error: Option<SharedString>,
}

/// The console of an operation under way: what `wt` says, as it says it.
///
/// Plain data on the application, read by the dialog's render: a `new` clones
/// databases for minutes, and the lines are what stands between the user and a
/// spinner. Kept on success only until the dialog closes; on failure it stays,
/// because the steps that led to the error are the one account there is of it.
pub struct Console {
    pub op: wt::Op,
    /// Each line, and whether it was a warning.
    pub lines: Vec<(String, bool)>,
    /// The operation ended badly: the dialog stays, with a close button.
    pub failed: bool,
    /// Follows the tail, as a terminal does.
    pub scroll: gpui::ScrollHandle,
}

impl Console {
    fn new(op: wt::Op) -> Self {
        Self {
            op,
            lines: Vec::new(),
            failed: false,
            scroll: gpui::ScrollHandle::new(),
        }
    }
}

/// Where the dialog is.
pub enum Stage {
    /// The first page of a creation.
    Setup(Setup),
    /// The project's questions, one page per round.
    Questions,
    /// The operation runs, and the dialog is its console.
    Running(Console),
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
    pub stage: Stage,
    /// Shared and not owned: a menu's closure needs the choices of the question
    /// it belongs to, and it is rebuilt on every frame of the dialog — eighty
    /// tenants cloned three strings each, sixty times a second.
    pub questions: Rc<Vec<wt::Question>>,
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

impl WtPrompt {
    /// A dialog at its first stage, with nothing asked yet.
    fn new(main: PathBuf, slug: String, target: WtTarget, stage: Stage) -> Self {
        Self {
            main,
            slug,
            target,
            stage,
            questions: Rc::new(Vec::new()),
            answers: BTreeMap::new(),
            inputs: BTreeMap::new(),
            filters: BTreeMap::new(),
            asking: false,
            round: 0,
        }
    }
}

/// The `Done` or `Failed` that closes a `wt` operation carries an `Action`,
/// and the console an `Op`: which action ends which console. `Worktree` is
/// shared by `new` and `rm` — and by the bare git gestures, which never open a
/// console, so the match is read on the console's side only.
fn op_of(action: Action) -> Option<&'static [wt::Op]> {
    match action {
        Action::Worktree => Some(&[wt::Op::New, wt::Op::Remove][..]),
        Action::WtUp => Some(&[wt::Op::Up][..]),
        Action::WtDown => Some(&[wt::Op::Down][..]),
        _ => None,
    }
}

/// The inverse: the action a console's operation is started under, which is
/// what `start` puts in flight and what `Done` will echo.
fn action_of(op: wt::Op) -> Action {
    match op {
        wt::Op::New | wt::Op::Remove => Action::Worktree,
        wt::Op::Up => Action::WtUp,
        wt::Op::Down => Action::WtDown,
    }
}

/// The row of the branch list that stands for "a new branch".
const NEW_BRANCH: &str = "";

/// What an entry does when it is chosen.
///
/// Shared and not owned: the list is folded twice — into a menu and into rows —
/// and each rendering keeps its own handle on the same gesture.
pub(super) type WtRun =
    std::rc::Rc<dyn Fn(&mut ClaudhubApp, &mut Window, &mut Context<ClaudhubApp>)>;

/// One thing one can ask of a worktree.
///
/// Data and not a menu entry: the same list is folded into the top bar's
/// `PopupMenu` and painted as rows inside the worktree picker, whose scrolling
/// surface cannot carry a popup at all — gpui clips a deferred draw to the
/// content mask it was prepainted under.
pub(super) struct WtAction {
    pub(super) icon: &'static str,
    pub(super) label: SharedString,
    /// A rule is drawn above this entry: it opens a new group.
    pub(super) group: bool,
    /// The id a list needs to give the row, which two tasks of the same project
    /// must not share.
    pub(super) id: SharedString,
    pub(super) run: WtRun,
}

impl WtAction {
    fn new(
        id: impl Into<SharedString>,
        icon: &'static str,
        label: SharedString,
        run: impl Fn(&mut ClaudhubApp, &mut Window, &mut Context<ClaudhubApp>) + 'static,
    ) -> Self {
        Self {
            icon,
            label,
            group: false,
            id: id.into(),
            run: std::rc::Rc::new(run),
        }
    }

    fn group(mut self) -> Self {
        self.group = true;
        self
    }
}

/// The rows of the "open" menu under the static address: what `[open]
/// source` enumerated, or the state of the question.
///
/// Three states and each is said: not asked or not yet answered — an
/// ellipsis; answered with nothing — a sentence, because an empty block under
/// a rule reads as a glitch; answered — one row per address, the label first,
/// the address dimmed behind it, the row opening it.
fn wt_links_rows(links: Option<Option<Vec<wt::Endpoint>>>, cx: &App) -> gpui::AnyElement {
    let muted = cx.theme().muted_foreground;
    let note = |text: SharedString| {
        div()
            .px_2()
            .py_1()
            .text_sm()
            .text_color(muted)
            .child(text)
            .into_any_element()
    };
    let links = match links {
        None | Some(None) => return note(tr!("wt-links-resolving")),
        Some(Some(links)) if links.is_empty() => return note(tr!("wt-links-none")),
        Some(Some(links)) => links,
    };
    let accent = cx.theme().accent;
    v_flex()
        .w_full()
        .children(links.into_iter().enumerate().map(|(ix, link)| {
            let url = link.url.clone();
            h_flex()
                .id(("wt-link", ix))
                .w_full()
                .px_2()
                .py_1()
                .gap_2()
                .items_center()
                .rounded(cx.theme().radius)
                .cursor_pointer()
                .hover(|s| s.bg(accent.opacity(0.4)))
                .child(div().text_sm().child(SharedString::from(link.label)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(muted)
                        .child(SharedString::from(link.url)),
                )
                .on_click(move |_, _window, cx| cx.open_url(&url))
        }))
        .into_any_element()
}

/// The resolved links of one worktree, as the "open" menu shows them.
///
/// An entity of its own for one reason: it is painted inside a popover, whose
/// content is built from `ClaudhubApp::render`, and what has to read the
/// application must be a child whose `render` runs after the parent's. The
/// request for the links is **deferred** rather than sent from the constructor,
/// which runs in the same place.
pub(super) struct WtLinksMenu {
    app: gpui::WeakEntity<ClaudhubApp>,
    worktree: PathBuf,
}

impl WtLinksMenu {
    fn new(
        app: gpui::WeakEntity<ClaudhubApp>,
        main: PathBuf,
        worktree: PathBuf,
        slug: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let requester = app.clone();
        let target = worktree.clone();
        cx.defer(move |cx| {
            let _ = requester.update(cx, |app, _| app.request_wt_links(main, target, slug));
        });
        Self { app, worktree }
    }
}

impl Render for WtLinksMenu {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let links = self
            .app
            .upgrade()
            .and_then(|app| app.read(cx).wt_links.get(&self.worktree).cloned());
        wt_links_rows(links, cx)
    }
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

    /// Reads it again — the file has just changed.
    ///
    /// The entry is **kept** while the answer travels, as `reload_just` keeps
    /// the justfile's: the project's gestures would blink out of the menu on
    /// every save, and what they say is right until the new reading
    /// contradicts it. A repository never asked about is not asked now: the
    /// first read belongs to the menu that needs it.
    pub(super) fn reload_wt_project(&mut self, main: &Path) {
        if !self.wt_projects.contains_key(main) {
            return;
        }
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

    /// Asks the worker what `[open] source` enumerates for a worktree.
    ///
    /// Called when the "open" menu opens, and each time: the list is one per
    /// tenant mounted, and a tenant added since the last look has to show. What
    /// was resolved before **stays on screen** while the new answer travels —
    /// the entry only becomes "pending" when there was nothing yet — so the
    /// menu never empties under the pointer.
    pub(super) fn request_wt_links(&mut self, main: PathBuf, worktree: PathBuf, slug: String) {
        self.wt_links.entry(worktree.clone()).or_insert(None);
        self.git.send(Cmd::WtLinks {
            main,
            worktree,
            slug,
        });
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

    /// Starts the guided creation of a worktree, the folder already named.
    ///
    /// The way in for a repository with no `wt.toml`, where the text dialog
    /// asks the one thing there is to ask; a project goes through
    /// [`Self::setup_worktree`], which asks the rest first. Without a project,
    /// we fall back to the bare git add: a repository with no configuration
    /// must still be able to gain a worktree.
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
        if self.wt_project(&main).is_none() {
            self.add_worktree_without_wt(&main, &slug, from.as_deref(), cx);
            return;
        }
        let target = WtTarget::Create { branch: None, from };
        self.creation = Some(WtPrompt::new(main, slug, target, Stage::Questions));
        self.open_creation_dialog(window, cx);
        self.after_setup(window, cx);
    }

    /// Opens the creation dialog on its first page: the branch, where it
    /// starts, the folder — what `wt`'s own interface asks before the project's
    /// questions, and what Claudhub used to decide alone, always a new
    /// `wt/<name>` off HEAD.
    ///
    /// `branch` is the picker's "worktree from this branch": the page opens on
    /// it, with its slug suggested, and the rest of the creation is the same
    /// road — which is the point. That gesture used to go straight to git, and
    /// a worktree made that way had no ports, no copies, no `post_new`, and was
    /// then taken for one `wt` knew, since it sat under the project's root.
    pub(super) fn setup_worktree(
        &mut self,
        main: PathBuf,
        branch: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let suggested = branch.as_deref().map(wt::suggest_slug).unwrap_or_default();
        let slug = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(tr!("worktree-new-placeholder"))
                .default_value(suggested)
        });
        let mut filter = |cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(tr!("worktree-filter-branches")))
        };
        let setup = Setup {
            branch,
            from: None,
            slug: slug.clone(),
            branch_filter: filter(cx),
            base_filter: filter(cx),
            error: None,
        };
        let target = WtTarget::Create {
            branch: None,
            from: None,
        };
        self.creation = Some(WtPrompt::new(
            main.clone(),
            String::new(),
            target,
            Stage::Setup(setup),
        ));
        // The list is re-read rather than trusted: a branch fetched since the
        // last reading is exactly the one this page is opened for.
        self.git.send(Cmd::LoadBranches { main });
        self.open_creation_dialog(window, cx);
        crate::ui::dialogs::focus_field(&slug, window, cx);
        cx.notify();
    }

    /// The first page is answered: the folder is checked, and the project's
    /// questions come next — or the creation itself, when it asks none.
    fn submit_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(creation) = self.creation.as_mut() else {
            return;
        };
        let Stage::Setup(setup) = &mut creation.stage else {
            return;
        };
        let slug = setup.slug.read(cx).value().trim().to_string();
        // Said in the dialog and not in a balloon: the field is right there,
        // and the rule — lowercase, digits, dashes — is the one thing to read
        // before typing again.
        if !wt::slug_is_valid(&slug) {
            setup.error = Some(tr!("worktree-slug-invalid"));
            cx.notify();
            return;
        }
        creation.slug = slug;
        creation.target = WtTarget::Create {
            branch: setup.branch.clone(),
            from: setup.from.clone(),
        };
        creation.stage = Stage::Questions;
        self.after_setup(window, cx);
    }

    /// After the folder is known: the project's questions, if it asks any, or
    /// the creation itself.
    fn after_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(main) = self.creation.as_ref().map(|creation| creation.main.clone()) else {
            return;
        };
        let asks = self
            .wt_project(&main)
            .is_some_and(|project| project.has_new_prompts);
        if !asks {
            self.run_wt_target(window, cx);
            return;
        }
        let Some(creation) = self.creation.as_mut() else {
            return;
        };
        creation.asking = true;
        creation.round = 0;
        self.git.send(Cmd::WtQuestions {
            main: creation.main.clone(),
            slug: creation.slug.clone(),
            answers: BTreeMap::new(),
            phase: wt::Phase::New,
            task: None,
            round: 0,
        });
        cx.notify();
    }

    /// Chooses the branch of the first page, and suggests its folder.
    ///
    /// The suggestion **replaces** what the field holds: that is what a
    /// suggestion is, and the field stays editable afterwards. A branch already
    /// checked out elsewhere cannot be chosen — git refuses two checkouts of
    /// one branch, and the row says so instead of letting the creation fail.
    fn choose_setup_branch(
        &mut self,
        branch: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(creation) = self.creation.as_mut() else {
            return;
        };
        let Stage::Setup(setup) = &mut creation.stage else {
            return;
        };
        let suggested = branch.as_deref().map(wt::suggest_slug).unwrap_or_default();
        setup.branch = branch;
        setup.error = None;
        setup
            .slug
            .update(cx, |state, cx| state.set_value(suggested, window, cx));
        cx.notify();
    }

    fn choose_setup_base(&mut self, from: String, cx: &mut Context<Self>) {
        let Some(creation) = self.creation.as_mut() else {
            return;
        };
        if let Stage::Setup(setup) = &mut creation.stage {
            setup.from = Some(from);
            cx.notify();
        }
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
        let mut prompt = WtPrompt::new(main.clone(), slug.clone(), target, Stage::Questions);
        prompt.asking = true;
        self.creation = Some(prompt);
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

    /// Opens the dialog straight on its console, for an operation that asks
    /// nothing: `down`, `rm`, and an `up` whose project has no question.
    fn run_in_console(
        &mut self,
        main: PathBuf,
        slug: String,
        op: wt::Op,
        cmd: Cmd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let action = action_of(op);
        // The target is only read for the phase of a question round, which a
        // console never asks for; `Create` is the neutral one.
        let target = WtTarget::Create {
            branch: None,
            from: None,
        };
        self.creation = Some(WtPrompt::new(
            main,
            slug,
            target,
            Stage::Running(Console::new(op)),
        ));
        self.open_creation_dialog(window, cx);
        self.start(None, action, cmd, cx);
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
        creation.questions = Rc::new(questions);
        creation.inputs = inputs;
        creation.filters = filters;
        // The round's first text field takes the focus: the questions arrive
        // after the dialog opens, so this is the only moment there is a field to
        // put the caret in. The choice filters are left alone — a filter is not
        // what the page asks for, and Enter in it would answer the whole page.
        let first = creation
            .questions
            .iter()
            .find_map(|question| creation.inputs.get(&question.name))
            .cloned();
        if let Some(field) = first {
            crate::ui::dialogs::focus_field(&field, window, cx);
        }
        cx.notify();
    }

    /// Adds or removes one value of a multiple choice.
    ///
    /// The answer is read here and not carried into the box's closure: a
    /// question whose choices come from a shell command has eighty of them, and
    /// each box would otherwise hold a copy of everything chosen.
    fn toggle_multi(&mut self, name: &str, value: &str, separator: &str, cx: &mut Context<Self>) {
        let Some(creation) = self.creation.as_ref() else {
            return;
        };
        let current = creation
            .answers
            .get(name)
            .map(String::as_str)
            .unwrap_or_default();
        let mut chosen: Vec<&str> = current
            .split(separator)
            .filter(|part| !part.is_empty())
            .collect();
        if let Some(at) = chosen.iter().position(|part| *part == value) {
            chosen.remove(at);
        } else {
            chosen.push(value);
        }
        let joined = chosen.join(separator);
        self.answer(name.to_string(), joined, cx);
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
    ///
    /// A creation and a start **keep the dialog**, which becomes their console:
    /// both run the project's hooks, and what the hooks say is what the next
    /// minutes are made of. A task goes to a terminal, and the dialog closes.
    fn run_wt_target(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(creation) = self.creation.as_mut() else {
            return;
        };
        let (main, slug, answers) = (
            creation.main.clone(),
            creation.slug.clone(),
            std::mem::take(&mut creation.answers),
        );
        match creation.target.clone() {
            WtTarget::Create { branch, from } => {
                creation.stage = Stage::Running(Console::new(wt::Op::New));
                self.start(
                    None,
                    Action::Worktree,
                    Cmd::WtCreate {
                        main,
                        slug,
                        branch,
                        from,
                        answers,
                    },
                    cx,
                );
            }
            WtTarget::Up { worktree } => {
                creation.stage = Stage::Running(Console::new(wt::Op::Up));
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
                self.creation = None;
                window.close_all_dialogs(cx);
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

    /// One line of a `wt` operation, as the worker relays it.
    ///
    /// Filed only in the console it belongs to: the operation is named by its
    /// repository, its folder and its kind, and a line of a `down` finishing
    /// late has no business in the console of the `up` opened after it. A
    /// console one has hidden, or never opened, drops the line — the journal
    /// has it.
    pub(super) fn wt_progress(
        &mut self,
        main: &Path,
        slug: &str,
        op: wt::Op,
        line: String,
        warning: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(creation) = self.creation.as_mut() else {
            return;
        };
        if creation.main != main || creation.slug != slug {
            return;
        }
        let Stage::Running(console) = &mut creation.stage else {
            return;
        };
        if console.op != op {
            return;
        }
        console.lines.push((line, warning));
        // The tail, as a terminal shows it: the line that matters is the one
        // just said, and a list that stays on its first lines hides it.
        console.scroll.scroll_to_bottom();
        cx.notify();
    }

    /// A `wt` operation has ended: the console closes on success, the balloon
    /// saying the rest; it stays on failure, with what was said before the
    /// error and a button to close it.
    ///
    /// Read on the **console's** side: `Action::Worktree` is also the bare git
    /// gestures, which never open one, and a success that finds no console
    /// does nothing.
    pub(super) fn wt_operation_ended(
        &mut self,
        action: Action,
        ok: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ops) = op_of(action) else {
            return;
        };
        let Some(creation) = self.creation.as_mut() else {
            return;
        };
        let Stage::Running(console) = &mut creation.stage else {
            return;
        };
        if !ops.contains(&console.op) {
            return;
        }
        if ok {
            self.creation = None;
            window.close_all_dialogs(cx);
        } else {
            console.failed = true;
        }
        cx.notify();
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

    /// Enter, or the OK button, at whatever stage the dialog is.
    ///
    /// Returns whether the dialog closes: it stays through the pages and while
    /// the operation runs, and the one Enter that closes it is the one on a
    /// console — hiding a running one, or dismissing a failed one.
    fn confirm_creation(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(creation) = self.creation.as_ref() else {
            return true;
        };
        match &creation.stage {
            Stage::Setup(_) => {
                self.submit_setup(window, cx);
                false
            }
            Stage::Questions => {
                // A page that is still being fetched has nothing to confirm:
                // Enter would send the previous answers a second time.
                if !creation.asking {
                    self.submit_answers(cx);
                }
                false
            }
            Stage::Running(_) => {
                self.creation = None;
                true
            }
        }
    }

    /// The creation dialog.
    ///
    /// It redraws on every frame from `Creation`: the questions arrive after it
    /// opens, and content frozen at construction would stay empty. The title
    /// and the buttons too — the same dialog is a form, then a console.
    fn open_creation_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entity = cx.entity();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let entity = entity.clone();
            let (ok, cancel) = (entity.clone(), entity.clone());
            let (title, footer, body) = entity.update(_cx, |this, cx| {
                (
                    this.creation_title(),
                    this.creation_footer(),
                    this.render_creation_body(cx).into_any_element(),
                )
            });
            dialog
                .title(title)
                .child(div().w(px(520.)).child(body))
                .overlay_closable(false)
                .close_button(false)
                .footer(footer)
                .on_ok(move |_, window, cx| {
                    ok.update(cx, |this, cx| this.confirm_creation(window, cx))
                })
                .on_cancel(move |_, _window, cx| {
                    // Escape on a console hides it; the operation goes on, and
                    // the balloon says how it ended.
                    cancel.update(cx, |this, _| this.creation = None);
                    true
                })
        });
    }

    /// The title says which gesture the dialog belongs to: the same dialog
    /// serves five, and "New worktree" above the tenants of a `wt up` would be
    /// a plain lie.
    fn creation_title(&self) -> SharedString {
        let Some(creation) = self.creation.as_ref() else {
            return tr!("worktree-new-title");
        };
        match (&creation.stage, &creation.target) {
            (Stage::Running(console), _) => match console.op {
                wt::Op::New => tr!("worktree-new-title"),
                wt::Op::Up => tr!("worktree-up-title"),
                wt::Op::Down => tr!("worktree-down-title"),
                wt::Op::Remove => tr!("worktree-remove-wt-title"),
            },
            (_, WtTarget::Up { .. }) => tr!("worktree-up-title"),
            (_, WtTarget::Task { task, .. }) => SharedString::from(task.clone()),
            (_, WtTarget::Create { .. }) => tr!("worktree-new-title"),
        }
    }

    /// The buttons: cancel and OK through the pages, one button on a console —
    /// "hide" while it runs, "close" once it has failed.
    fn creation_footer(&self) -> gpui_component::dialog::DialogFooter {
        match self.creation.as_ref().map(|creation| &creation.stage) {
            Some(Stage::Running(console)) if console.failed => super::dialogs::close(),
            Some(Stage::Running(_)) => super::dialogs::only(tr!("worktree-console-hide")),
            _ => super::dialogs::confirm(),
        }
    }

    fn render_creation_body(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(creation) = self.creation.as_ref() else {
            return div().into_any_element();
        };
        match &creation.stage {
            Stage::Setup(_) => return self.render_setup(cx).into_any_element(),
            Stage::Running(_) => return self.render_console(cx).into_any_element(),
            Stage::Questions => {}
        }
        if creation.asking {
            return div()
                .p_4()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(tr!("worktree-asking"))
                .into_any_element();
        }
        // Shared, and the answers and the fields merely borrowed: all three
        // were copied whole on every frame of the dialog, and the questions
        // carry every choice of every one of them.
        let questions = creation.questions.clone();
        let muted = cx.theme().muted_foreground;

        let mut rows = Vec::new();
        for (index, question) in questions.iter().enumerate() {
            let current = creation
                .answers
                .get(&question.name)
                .map(String::as_str)
                .unwrap_or_default();
            let field = match question.kind {
                wt::Kind::Text => creation
                    .inputs
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
                    .render_choice(&questions, index, current, cx)
                    .into_any_element(),
                wt::Kind::Multi => self.render_multi(question, current, cx).into_any_element(),
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

    /// The first page: the branch, where it starts, the folder.
    fn render_setup(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(creation) = self.creation.as_ref() else {
            return div().into_any_element();
        };
        let Stage::Setup(setup) = &creation.stage else {
            return div().into_any_element();
        };
        // Borrowed, not copied: this runs on every frame of the dialog, and a
        // repository has a hundred branches carrying a subject each.
        let branches: &[crate::git::Branch] = self
            .repos
            .iter()
            .find(|repo| repo.main == creation.main)
            .map(|repo| repo.branches.as_slice())
            .unwrap_or_default();
        let head = branches
            .iter()
            .find(|branch| branch.is_head)
            .map(|branch| branch.name.clone());
        let danger = cx.theme().danger;

        let mut page = v_flex().gap_3();

        // Which branch. "New" first, then the list the top bar's picker shows,
        // a branch already checked out greyed and saying where.
        page = page.child(
            v_flex()
                .gap_1()
                .child(div().text_sm().child(tr!("worktree-setup-branch")))
                .child(self.render_branch_list(
                    "wt-setup-branch",
                    branches,
                    &setup.branch_filter,
                    Some(setup.branch.as_deref().unwrap_or(NEW_BRANCH)),
                    true,
                    head.as_deref(),
                    |this, name, window, cx| {
                        let branch = (name != NEW_BRANCH).then(|| name.to_string());
                        this.choose_setup_branch(branch, window, cx);
                    },
                    cx,
                )),
        );

        // Where a new branch starts: every branch, HEAD first chosen. Only for
        // a new one — an existing branch starts where it is.
        if setup.branch.is_none() && !branches.is_empty() {
            let selected = setup.from.clone().or_else(|| head.clone());
            page = page.child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().child(tr!("worktree-setup-base")))
                    .child(self.render_branch_list(
                        "wt-setup-base",
                        branches,
                        &setup.base_filter,
                        selected.as_deref(),
                        false,
                        head.as_deref(),
                        |this, name, _window, cx| this.choose_setup_base(name.to_string(), cx),
                        cx,
                    )),
            );
        }

        page = page.child(
            v_flex()
                .gap_1()
                .child(div().text_sm().child(tr!("worktree-setup-slug")))
                .child(Input::new(&setup.slug).small())
                .when_some(setup.error.clone(), |el, error| {
                    el.child(div().text_xs().text_color(danger).child(error))
                }),
        );
        page.into_any_element()
    }

    /// A list of branches to pick one from, with a search field past
    /// [`FILTER_ABOVE`] and a bounded height — the shape of a long multiple
    /// choice, for the same reason: a repository has a hundred branches, and a
    /// hundred rows with no way to narrow them is a list one scrolls past.
    ///
    /// `with_new` puts "a new branch" first and greys the branches already
    /// checked out; a base list has neither — a branch checked out elsewhere is
    /// a perfectly good start point.
    #[allow(clippy::too_many_arguments)]
    fn render_branch_list(
        &self,
        id: &'static str,
        branches: &[crate::git::Branch],
        filter: &Entity<InputState>,
        selected: Option<&str>,
        with_new: bool,
        head: Option<&str>,
        choose: impl Fn(&mut Self, &str, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let choose = Rc::new(choose);
        let needle = filter.read(cx).value().to_string();
        let long = branches.len() > FILTER_ABOVE;
        let muted = cx.theme().muted_foreground;
        let accent = cx.theme().accent;
        let radius = cx.theme().radius;

        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        let row = |index: usize,
                   name: String,
                   label: SharedString,
                   detail: SharedString,
                   taken: bool,
                   cx: &mut Context<Self>| {
            let is_selected = selected == Some(name.as_str());
            let entity = cx.entity();
            let choose = choose.clone();
            h_flex()
                .id(SharedString::from(format!("{id}-{index}")))
                .w_full()
                .px_2()
                .py_1()
                .gap_2()
                .items_baseline()
                .rounded(radius)
                .when(!taken, |el| {
                    el.cursor_pointer()
                        .hover(|s| s.bg(accent.opacity(0.5)))
                        .on_click(move |_, window, cx| {
                            let name = name.clone();
                            entity.update(cx, |this, cx| choose(this, &name, window, cx));
                        })
                })
                .when(is_selected, |el| el.bg(accent))
                .when(taken, |el| el.opacity(0.5))
                .child(
                    div()
                        .flex_none()
                        .text_sm()
                        .when(is_selected, |el| el.font_semibold())
                        .child(label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(muted)
                        .child(detail),
                )
                .into_any_element()
        };
        if with_new {
            rows.push(row(
                0,
                NEW_BRANCH.to_string(),
                tr!("worktree-setup-new-branch"),
                tr!("worktree-setup-new-branch-hint"),
                false,
                cx,
            ));
        }
        for (index, entry) in crate::ui::branches::rows_for(branches, &needle)
            .into_iter()
            .enumerate()
        {
            match entry {
                crate::ui::branches::Row::Group(kind) => rows.push(
                    div()
                        .px_2()
                        .pt_1()
                        .text_xs()
                        .text_color(muted)
                        .child(match kind {
                            crate::git::BranchKind::Local => tr!("branches-local"),
                            crate::git::BranchKind::Remote => tr!("branches-remote"),
                        })
                        .into_any_element(),
                ),
                crate::ui::branches::Row::Branch(branch) => {
                    let taken = with_new && branch.taken();
                    let detail = match (&branch.taken_by, head == Some(branch.name.as_str())) {
                        (Some(path), _) if taken => tr!("worktree-setup-taken", {
                            path: path.display().to_string()
                        }),
                        (_, true) => SharedString::from(format!(
                            "{} · {}",
                            tr!("worktree-setup-head"),
                            branch.detail
                        )),
                        _ => SharedString::from(branch.detail.clone()),
                    };
                    rows.push(row(
                        index + 1,
                        branch.name.clone(),
                        SharedString::from(branch.name.clone()),
                        detail,
                        taken,
                        cx,
                    ));
                }
            }
        }

        v_flex()
            .gap_1()
            .when(long, |el| el.child(div().child(Input::new(filter).small())))
            .child(
                div()
                    .id(SharedString::from(id))
                    .max_h(px(200.))
                    .overflow_y_scroll()
                    .child(v_flex().gap_px().children(rows)),
            )
    }

    /// The console: every line the operation has said, warnings in colour, the
    /// tail kept in view, and the error's place at the bottom when it failed.
    fn render_console(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(creation) = self.creation.as_ref() else {
            return div().into_any_element();
        };
        let Stage::Running(console) = &creation.stage else {
            return div().into_any_element();
        };
        let (muted, warning, danger) = (
            cx.theme().muted_foreground,
            cx.theme().warning,
            cx.theme().danger,
        );
        let mono = cx.theme().mono_font_family.clone();
        let lines = console.lines.iter().map(|(line, warn)| {
            div()
                .text_xs()
                .font_family(mono.clone())
                .when(*warn, |el| el.text_color(warning))
                .child(SharedString::from(line.clone()))
        });
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .text_color(muted)
                    .when(!console.failed, |el| {
                        el.child(icon("loader-circle").xsmall())
                            .child(tr!("worktree-console-running"))
                    })
                    .when(console.failed, |el| {
                        el.text_color(danger).child(tr!("worktree-console-failed"))
                    })
                    .child(div().flex_1())
                    .child(SharedString::from(creation.slug.clone())),
            )
            .child(
                div()
                    .id("wt-console")
                    .max_h(px(320.))
                    .min_h(px(80.))
                    .overflow_y_scroll()
                    .track_scroll(&console.scroll)
                    .p_2()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().muted.opacity(0.3))
                    .child(v_flex().gap_px().children(lines)),
            )
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
        questions: &Rc<Vec<wt::Question>>,
        index: usize,
        current: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let question = &questions[index];
        if question.choices.len() > ROWS_UP_TO {
            return self
                .render_choice_menu(questions, index, current, cx)
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
        questions: &Rc<Vec<wt::Question>>,
        index: usize,
        current: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let question = &questions[index];
        let label = question
            .choices
            .iter()
            .find(|choice| choice.value == current)
            .map(|choice| choice.label.clone())
            .unwrap_or_else(|| current.to_string());
        let entity = cx.entity();
        // The question is named by its index into the shared list rather than
        // copied into the closure: this closure is rebuilt on every frame, and
        // a long choice — a `source` listing eighty tenants — is exactly the
        // case this menu exists for.
        let (name, questions) = (question.name.clone(), questions.clone());
        Button::new(SharedString::from(format!("wt-choice-{}", question.name)))
            .outline()
            .small()
            .label(SharedString::from(label))
            .dropdown_menu(move |menu, _window, _cx| {
                questions[index].choices.iter().fold(menu, |menu, choice| {
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
        let chosen: Vec<&str> = current
            .split(separator.as_str())
            .filter(|value| !value.is_empty())
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
            let checked = chosen.iter().any(|value| *value == choice.value);
            let entity = cx.entity();
            let (name, value, separator) = (
                question.name.clone(),
                choice.value.clone(),
                separator.clone(),
            );
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
                        // The answer is read again when the box is clicked
                        // rather than carried here: a copy of it per box per
                        // frame is eighty vectors of eighty strings on the
                        // question this list exists for.
                        .on_click(move |_, _window, cx| {
                            entity.update(cx, |this, cx| {
                                this.toggle_multi(&name, &value, &separator, cx)
                            });
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

        // Nothing chosen is said, not refused: `wt` accepts an empty answer and
        // the project decides what it means — on Acetics, no tenant mounted —
        // but it is the answer one gives by mistake, by confirming a page one
        // has not read.
        let warning = cx.theme().warning;
        v_flex()
            .gap_1()
            .when(chosen.is_empty(), |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(warning)
                        .child(tr!("worktree-multi-empty")),
                )
            })
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
        // seconds, which the console shows as they go.
        self.flight.set_wt_target(worktree.to_path_buf());
        let cmd = Cmd::WtUp {
            main: main.clone(),
            slug: slug.clone(),
            answers: BTreeMap::new(),
        };
        self.run_in_console(main, slug, wt::Op::Up, cmd, window, cx);
    }

    pub(super) fn wt_down(
        &mut self,
        main: PathBuf,
        worktree: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(slug) = self.wt_slug(&main, worktree) else {
            return;
        };
        self.flight.set_wt_target(worktree.to_path_buf());
        let cmd = Cmd::WtDown {
            main: main.clone(),
            slug: slug.clone(),
        };
        self.run_in_console(main, slug, wt::Op::Down, cmd, window, cx);
    }

    pub(super) fn wt_remove(
        &mut self,
        main: PathBuf,
        worktree: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(slug) = self.wt_slug(&main, worktree) else {
            return;
        };
        // `worktrees_changed` answers without naming a checkout — the one that
        // was removed no longer exists — so the key is the bare action.
        let cmd = Cmd::WtRemove {
            main: main.clone(),
            slug: slug.clone(),
        };
        self.run_in_console(main, slug, wt::Op::Remove, cmd, window, cx);
    }

    // — `just` ————————————————————————————————————————————————

    /// Reads this checkout's justfile, once.
    ///
    /// Marked as read straight away, like `wt.toml`: without that, every frame
    /// of the bar would restart the subprocess for the whole length of the read.
    pub(super) fn ensure_just(&mut self, worktree: &Path) {
        if self.just_recipes.contains_key(worktree) {
            return;
        }
        self.just_recipes.insert(worktree.to_path_buf(), None);
        self.git.send(Cmd::JustLoad {
            worktree: worktree.to_path_buf(),
        });
    }

    /// Reads it again — the file has just changed.
    ///
    /// The entry is **kept** while the answer travels rather than cleared: the
    /// button would blink out and back on every save, and what it says is right
    /// until the new reading contradicts it.
    pub(super) fn reload_just(&mut self, worktree: &Path) {
        self.git.send(Cmd::JustLoad {
            worktree: worktree.to_path_buf(),
        });
    }

    /// What this checkout's justfile offers, when it has one with a recipe in
    /// it.
    ///
    /// The handle is shared, not the snapshot copied: the bar reads it on every
    /// frame and the run menu keeps it in a `'static` closure.
    pub(super) fn just_recipes(
        &self,
        worktree: &Path,
    ) -> Option<std::rc::Rc<crate::just::Snapshot>> {
        let snapshot = self.just_recipes.get(worktree)?.clone()?;
        snapshot.default.is_some().then_some(snapshot)
    }

    /// Runs a recipe in a terminal tab.
    ///
    /// A terminal and not an output panel, for the reason the project's tasks
    /// are already there: a recipe builds, watches, serves or asks something —
    /// `just run` opens a window — and what it writes is coloured, progressive,
    /// and sometimes answered.
    pub(super) fn run_just(
        &mut self,
        worktree: PathBuf,
        recipe: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let line = format!("just {}", crate::cmdline::join_command([&recipe]));
        // `open_terminal` shows the panel: asking for it here would open a shell
        // beside the recipe's own.
        self.open_terminal(
            &worktree,
            crate::ui::terminal_view::Launch {
                // Through a login shell, like a project task: `just` is looked
                // up on the `PATH`, and a window opened from a desktop launcher
                // does not have the one the user's shell builds.
                command: Some(("sh".into(), vec!["-lc".into(), line])),
                env: HashMap::new(),
                label: SharedString::from(format!("just {recipe}")),
                agent: false,
            },
            window,
            cx,
        );
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
        // `open_terminal` shows the panel: asking for it here would open a
        // shell beside the task's own.
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
                .on_ok(move |_, window, cx| {
                    entity.update(cx, |this, _cx| {
                        this.git.send(Cmd::DeleteBranch {
                            main: main.clone(),
                            name: branch.clone(),
                            force: false,
                        });
                    });
                    // The removal opens its console, and it opens **after**
                    // this dialog has closed: `true` closes the topmost one,
                    // which would be the console if it were opened here.
                    let (entity, main, worktree) = (entity.clone(), main.clone(), worktree.clone());
                    window.defer(cx, move |window, cx| {
                        entity.update(cx, |this, cx| this.wt_remove(main, &worktree, window, cx));
                    });
                    true
                })
        });
    }

    /// A worktree's context menu: git on one side, the project on the other.
    /// Everything one can ask of a worktree, as data.
    ///
    /// **One table, read twice**: the top bar's `…` folds it into a `PopupMenu`,
    /// the worktree picker paints it as rows in its second step. Two lists would
    /// have drifted at the first addition, and the drift is the silent kind —
    /// one of the two menus would simply lack an entry nobody notices missing.
    ///
    /// It is what the sidebar row's right click used to be, plus what a row
    /// cannot offer once there is no row: a terminal here, the path, the folder.
    pub(super) fn worktree_actions(
        &mut self,
        main: PathBuf,
        worktree: PathBuf,
        cx: &App,
    ) -> Vec<WtAction> {
        self.ensure_wt_project(&main);
        let project = self.wt_project(&main).cloned();
        let known = self.wt_slug(&main, &worktree).is_some();
        let mut actions = Vec::new();

        {
            let target = worktree.clone();
            actions.push(WtAction::new(
                "update",
                "arrow-down-to-line",
                tr!("worktree-update"),
                move |app, window, cx| {
                    // Updating from the base is read off the worktree being
                    // looked at: selecting it first is what makes the entry mean
                    // the row it was opened on and not the one on screen.
                    app.select_worktree(target.clone(), window, cx);
                    app.update_from_base(cx);
                },
            ));
        }
        {
            let target = worktree.clone();
            actions.push(WtAction::new(
                "integrate",
                "git-merge",
                tr!("worktree-integrate"),
                move |app, _window, cx| app.integrate(target.clone(), cx),
            ));
        }

        // What a row could offer and a picker still can: somewhere to type, the
        // path to paste elsewhere, the folder in the system's own browser.
        {
            let target = worktree.clone();
            actions.push(
                WtAction::new(
                    "terminal",
                    "terminal",
                    tr!("worktree-terminal"),
                    move |app, window, cx| app.work_in_worktree(&target, window, cx),
                )
                .group(),
            );
        }
        {
            let target = worktree.clone();
            actions.push(WtAction::new(
                "copy-path",
                "copy",
                tr!("action-copy-path"),
                move |_app, _window, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                        target.display().to_string(),
                    ));
                },
            ));
        }
        {
            let target = worktree.clone();
            actions.push(WtAction::new(
                "reveal",
                "folder-open",
                tr!("worktree-reveal"),
                move |_app, _window, cx| cx.open_url(&format!("file://{}", target.display())),
            ));
        }
        // Pinning: the one entry here that is not about the checkout but about
        // the window — it puts a button for it in the top bar, where switching
        // to it costs a click instead of a popover and a filter. It lives in
        // this table because that is where everything one asks of a worktree
        // lives, and the table is read twice.
        {
            let target = worktree.clone();
            let pinned = crate::ui::store::Store::global(cx).is_pinned(&worktree);
            actions.push(WtAction::new(
                "pin",
                if pinned { "pin-off" } else { "pin" },
                if pinned {
                    tr!("worktree-unpin")
                } else {
                    tr!("worktree-pin")
                },
                move |app, _window, cx| app.toggle_pin(&target, cx),
            ));
        }

        let Some(project) = project.filter(|_| known) else {
            // Removing the checkout itself, when `wt` is not the one keeping the
            // books. Not offered on the main worktree — git refuses, and the
            // entry would promise an error.
            if worktree != main {
                let (main, target) = (main.clone(), worktree.clone());
                actions.push(
                    WtAction::new(
                        "remove-checkout",
                        "trash-2",
                        tr!("worktree-remove-checkout"),
                        move |app, window, cx| {
                            app.confirm_remove_worktree(main.clone(), target.clone(), window, cx)
                        },
                    )
                    .group(),
                );
            }
            actions.push(Self::forget_action(main));
            return actions;
        };

        let mut first = true;
        if project.has_up {
            let (main, target) = (main.clone(), worktree.clone());
            actions.push(
                WtAction::new(
                    "wt-up",
                    "play",
                    tr!("worktree-up"),
                    move |app, window, cx| app.wt_up(main.clone(), &target, window, cx),
                )
                .group(),
            );
            first = false;
        }
        if project.has_down {
            let (main, target) = (main.clone(), worktree.clone());
            let action = WtAction::new(
                "wt-down",
                "circle-stop",
                tr!("worktree-down"),
                move |app, window, cx| app.wt_down(main.clone(), &target, window, cx),
            );
            actions.push(if first { action.group() } else { action });
            first = false;
        }
        {
            let (main, target) = (main.clone(), worktree.clone());
            let action = WtAction::new(
                "wt-remove",
                "trash-2",
                tr!("worktree-remove"),
                move |app, window, cx| app.wt_remove(main.clone(), &target, window, cx),
            );
            actions.push(if first { action.group() } else { action });
        }

        // The project's tasks, as it declares them. Claudhub does not know what
        // they do, and that is the point.
        for (index, task) in project.tasks.into_iter().enumerate() {
            let (main, target) = (main.clone(), worktree.clone());
            let name = task.name.clone();
            let label = if task.description.is_empty() {
                task.name.clone()
            } else {
                format!("{} — {}", task.name, task.description)
            };
            let action = WtAction::new(
                SharedString::from(format!("task-{}", task.name)),
                "terminal",
                SharedString::from(label),
                move |app, window, cx| {
                    app.wt_task(main.clone(), target.clone(), name.clone(), window, cx)
                },
            );
            actions.push(if index == 0 { action.group() } else { action });
        }

        actions.push(Self::forget_action(main));
        actions
    }

    /// Pins a checkout to the top bar, or takes it off.
    pub(super) fn toggle_pin(&mut self, worktree: &Path, cx: &mut Context<Self>) {
        let worktree = worktree.to_path_buf();
        crate::ui::store::Store::update_global(cx, move |store| store.toggle_pin(&worktree));
        cx.notify();
    }

    /// The pinned checkouts, in the order they were pinned, and only those a
    /// repository currently claims.
    ///
    /// A pin whose repository has not been opened yet is **kept in the store**
    /// and simply not painted: it comes back with its repository. Painting it
    /// would give the bar a button that selects a worktree the rest of the
    /// window knows nothing about.
    pub(super) fn pinned_worktrees(&self, cx: &App) -> Vec<PathBuf> {
        crate::ui::store::Store::global(cx)
            .pinned
            .iter()
            .filter(|path| self.repos.worktree(path).is_some())
            .cloned()
            .collect()
    }

    /// What can be done to a worktree, folded into a menu.
    pub(super) fn worktree_menu(
        &mut self,
        menu: gpui_component::menu::PopupMenu,
        main: PathBuf,
        worktree: PathBuf,
        cx: &mut Context<Self>,
    ) -> gpui_component::menu::PopupMenu {
        let entity = cx.entity();
        self.worktree_actions(main, worktree, cx)
            .into_iter()
            .fold(menu, |menu, action| {
                let entity = entity.clone();
                let run = action.run.clone();
                let menu = if action.group { menu.separator() } else { menu };
                menu.item(
                    PopupMenuItem::new(action.label)
                        .icon(icon(action.icon))
                        .on_click(move |_, window, cx| {
                            entity.update(cx, |this, cx| run(this, window, cx));
                        }),
                )
            })
    }

    /// Closing the repository, last in every worktree menu.
    ///
    /// It was the sidebar's right click on a repository heading, and it has to
    /// keep living somewhere: nothing on disk is touched, but everything opened
    /// in the repository is closed, which is not a gesture one makes twice a day
    /// — hence its place, last and behind a rule, rather than a button one
    /// brushes past.
    fn forget_action(main: PathBuf) -> WtAction {
        WtAction::new("forget", "x", tr!("repo-forget"), move |app, window, cx| {
            app.forget_repository(main.clone(), window, cx)
        })
        .group()
    }

    /// A worktree's "open" button, when the project exposes an address.
    ///
    /// Two shapes, decided by the `wt.toml`. Without `[open] source` the
    /// button opens the static URL and nothing is asked. With one it is a
    /// menu: the static URL at once, then what `source` enumerates — one
    /// address per tenant mounted, on Acetics — resolved **when the menu
    /// opens**, as the project's own comment asks, because it is an SQL query
    /// through a container and has no business running while a list is drawn.
    ///
    /// The menu's builder touches nothing of the application: it runs inside
    /// the root's render, where reading the root entity is a panic. The rows
    /// are a `PopupMenuItem::element`, painted by the menu's own entity after
    /// the root has handed back, which is where the resolved links are read —
    /// and where the request goes out, once per opening: the builder, called
    /// on every opening, only raises a flag the first frame of the rows lowers.
    pub(super) fn render_wt_links(
        &self,
        worktree: &Path,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let first = self.wt_state(worktree)?.endpoints.first()?.clone();
        let button = Button::new(SharedString::from(format!(
            "wt-open-{}",
            worktree.display()
        )))
        .ghost()
        .xsmall()
        .icon(icon("external-link"))
        .tooltip(SharedString::from(first.label.clone()));
        let main = self.repo_of(worktree).map(|repo| repo.main.clone());
        let slug = main.as_deref().and_then(|main| {
            self.wt_project(main)
                .is_some_and(|project| project.has_open_source)
                .then(|| self.wt_slug(main, worktree))
                .flatten()
        });
        let (Some(main), Some(slug)) = (main, slug) else {
            let url = first.url.clone();
            return Some(
                button
                    .on_click(cx.listener(move |_, _, _window, cx| {
                        cx.open_url(&url);
                    }))
                    .into_any_element(),
            );
        };
        let app = cx.entity().downgrade();
        let worktree = worktree.to_path_buf();
        Some(
            button
                .dropdown_menu(move |menu, _window, cx| {
                    // The rows are a **child entity**, and not an element built
                    // here: a popover's content runs inside `ClaudhubApp`'s own
                    // render, where reading the application — let alone asking
                    // it for the links — is the panic `open_dialog`'s closure
                    // already taught this repository. The child renders once
                    // the parent has given the borrow back, and it is built
                    // anew at each opening, which is what makes the request go
                    // out again.
                    let rows = cx.new(|cx| {
                        WtLinksMenu::new(
                            app.clone(),
                            main.clone(),
                            worktree.clone(),
                            slug.clone(),
                            cx,
                        )
                    });
                    menu.link(SharedString::from(first.label.clone()), first.url.clone())
                        .separator()
                        .item(PopupMenuItem::element(move |_window, _cx| {
                            rows.clone().into_any_element()
                        }))
                })
                .into_any_element(),
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
            self.close_files_of(worktree, window, cx);
            self.summaries.remove(worktree);
            self.forget_finders(worktree);
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
        // A project asks what `wt`'s own interface asks — the branch, its
        // start, the folder, then its questions. Without one there is only a
        // name to ask for, and the bare git add is enough.
        if self.wt_project(&main).is_some() {
            self.setup_worktree(main, None, window, cx);
            return;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A console is opened under an action and closed by the `Done` that
    /// echoes it: the two tables must agree, or a console would never close —
    /// the silent kind of failure, a dialog that stays.
    #[test]
    fn a_consoles_action_closes_its_operation() {
        for op in [wt::Op::New, wt::Op::Up, wt::Op::Down, wt::Op::Remove] {
            let ops = op_of(action_of(op)).unwrap_or_default();
            assert!(ops.contains(&op), "{op:?}");
        }
        // The bare git gestures share `Worktree` with `new` and `rm`, and a
        // `Commit` never ends a console.
        assert!(op_of(Action::Commit).is_none());
    }
}
