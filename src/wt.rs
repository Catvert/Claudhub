//! What a project's `wt.toml` adds to Claudhub.
//!
//! This is the extension system, and it costs nothing: a project declares the
//! folders to create, the files to inherit, its ports, its hooks and its
//! tasks, and Claudhub shows them **without knowing about them**. A Laravel
//! start command and a `cargo watch` go through the same code.
//!
//! `wt` is a dependency, not a subprocess: the repository is ours, and parsing
//! its CLI's output — aligned, coloured and translated — would amount to
//! reading what is made for a human.
//!
//! **Nothing here may be called from the interface thread.** A `[[prompt]]`
//! with `source` launches a shell, a `post_new` can take minutes, and `up`
//! starts containers. Everything goes through a worker.
//!
//! The split with the terminal, which is not obvious: what keeps books —
//! creation, removal, `up`, `down` — goes through the library, which allocates
//! the ports and writes the state; the `[tasks.*]`, for their part, are the
//! project's commands, often interactive, and go into a terminal tab, which
//! already knows how to forward keystrokes and render colours.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

// `::wt` names the library, not this module: both carry the same name, and the
// prefix removes the ambiguity for the reader as much as for the compiler.
use ::wt::config::{Ask, Project, PromptKind};
use ::wt::ops::App;
use ::wt::{ops, state, tmpl, util};

/// When the questions are asked.
///
/// The three moments `wt` itself knows, and it is what decides which
/// `[[prompt]]`s apply: `ask = "new"`, `"up"`, `"both"`, and `"task"` — the
/// last never asked by a phase, only by a task that names it. It also reaches
/// the project's `when` and `source` scripts as `WT_PHASE`, which the Acetics
/// configuration reads to decide whether to ask for its tenants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Phase {
    #[default]
    New,
    Up,
    Task,
}

impl Phase {
    fn ask(self) -> Ask {
        match self {
            Self::New => Ask::New,
            Self::Up => Ask::Up,
            Self::Task => Ask::Task,
        }
    }
}

/// What a project declares, cut down to what the view shows.
///
/// A snapshot of plain data rather than `wt`'s `App`: the view has no business
/// holding an object that knows how to launch shells, and rebuilding it on
/// every operation costs only one file read.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Snapshot {
    pub name: String,
    /// Directory that holds the project's worktrees.
    pub root: PathBuf,
    /// Branch name template, `wt/{{slug}}` by default.
    pub branch_template: String,
    pub tasks: Vec<TaskInfo>,
    pub has_up: bool,
    pub has_down: bool,
    pub has_open: bool,
    /// Does `[open]` also carry a `source` — a shell command enumerating more
    /// addresses. The view needs to know before clicking: with one the "open"
    /// button is a menu, without one it opens the URL and nothing is asked.
    pub has_open_source: bool,
    /// Does the project ask questions before creating a worktree, and before
    /// starting one. Two flags and not one: a project may ask nothing at `new`
    /// and everything at `up`, and opening an empty dialog to find that out
    /// would be a click for nothing.
    pub has_new_prompts: bool,
    pub has_up_prompts: bool,
    /// The language servers the project declares (`[lsp.<name>]`).
    ///
    /// `wt` does nothing with them — it neither starts nor supervises one — and
    /// neither does this module: they are carried through so the editor can
    /// start what the project's code wants. It is the first level of the
    /// extension system doing its job, one file and no new mechanism.
    pub lsp: Vec<crate::lsp::Server>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TaskInfo {
    pub name: String,
    pub description: String,
    /// The task names `[[prompt]]`s of its own, which are asked before it runs
    /// and whose answers become its arguments.
    pub prompts: bool,
}

/// A question declared by the project, with its choices already resolved.
///
/// The choices may come from a shell command (`source`): they are therefore
/// computed in the worker, never while drawing the dialog.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Question {
    pub name: String,
    pub title: String,
    pub kind: Kind,
    pub choices: Vec<Choice>,
    pub default: Option<String>,
    /// Separator for the values of a multiple choice.
    pub separator: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Kind {
    Choice,
    Multi,
    Confirm,
    Text,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Choice {
    pub value: String,
    pub label: String,
    pub detail: String,
}

/// What is needed to launch a task in a terminal tab.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Launch {
    /// The project's commands, templates already resolved.
    pub commands: Vec<String>,
    pub cwd: PathBuf,
    /// `WT_SLUG`, `WT_PORT_*`, `WT_OPT_*`: a slightly long hook reads better
    /// with environment variables than with substitutions.
    pub env: BTreeMap<String, String>,
}

impl Launch {
    /// The line to hand a shell. The commands are chained with `&&`: a
    /// multi-step task stops at the first that fails, as it would on the
    /// command line.
    pub fn shell_line(&self) -> String {
        self.commands.join(" && ")
    }
}

/// An address the project knows how to open.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Endpoint {
    pub url: String,
    pub label: String,
}

/// The four operations `wt` keeps books for, and the only ones whose progress
/// is streamed: each runs the project's hooks, and a hook is measured in
/// minutes.
///
/// On the wire and in the console's title both: the view names the operation
/// by it, and the worker tags every line it relays with it, so that a line of
/// a `down` that finishes late cannot land in the console of the `up` opened
/// after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Op {
    New,
    Up,
    Down,
    Remove,
}

impl Op {
    /// The word `wt`'s own command line uses, for the journal.
    fn name(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Up => "up",
            Self::Down => "down",
            Self::Remove => "rm",
        }
    }
}

/// What the worker says about an operation as it goes: one line, and whether
/// it is a warning. `Sync`, because the line is handed over from the thread
/// that drains `wt`'s channel, not from the one running the operation.
pub type Progress<'a> = &'a (dyn Fn(String, bool) + Sync);

/// The folder name suggested for a branch: `wt`'s own rule, so that the
/// interface and the command line suggest the same thing for `origin/feat/X`.
///
/// The remote prefix goes first: `origin-feat-x` is not the slug anyone
/// wants, and it is what the terminal interface strips too.
pub fn suggest_slug(branch: &str) -> String {
    ops::slugify(branch.trim_start_matches("origin/"))
}

/// Whether `wt` will accept this folder name: lowercase letters, digits and
/// dashes, neither first nor last. The rule is `wt`'s and read from it rather
/// than copied — `cmd_new` checks it again, and two rules drift.
pub fn slug_is_valid(slug: &str) -> bool {
    ops::validate_slug(slug).is_ok()
}

/// A repository's project, or `None` if it has no `wt.toml`.
///
/// Absence is the common case — most repositories have none — and is not worth
/// an error: `wt`'s gestures simply disappear from the menu.
fn app(main: &Path) -> Option<App> {
    let project = Project::load(main).ok()?;
    App::new(project).ok()
}

pub fn snapshot(main: &Path) -> Option<Snapshot> {
    let app = app(main)?;
    let config = &app.project.config;
    Some(Snapshot {
        name: app.project.name(),
        root: app.root.clone(),
        branch_template: config
            .branch
            .clone()
            .unwrap_or_else(|| "wt/{{slug}}".into()),
        tasks: config
            .tasks
            .iter()
            .map(|(name, task)| TaskInfo {
                name: name.clone(),
                description: task.description.clone(),
                prompts: !task.prompt.is_empty(),
            })
            .collect(),
        has_up: app.has_up(),
        has_down: app.has_down(),
        has_open: app.has_open(),
        has_open_source: config.open.source.is_some(),
        has_new_prompts: config.prompts.iter().any(|p| p.ask.covers(Ask::New)),
        has_up_prompts: config.prompts.iter().any(|p| p.ask.covers(Ask::Up)),
        lsp: config
            .lsp
            .iter()
            .map(|(name, server)| crate::lsp::Server {
                name: name.clone(),
                command: server.command.clone(),
                args: server.args.clone(),
                env: server.env.clone().into_iter().collect(),
                extensions: server.extensions.clone(),
                language_id: server.language_id(name).to_string(),
            })
            .collect(),
    })
}

/// A checkout's slug: the name of its folder under the project root.
///
/// `None` for the main repository and for any worktree placed elsewhere: `wt`
/// only knows what it created, and asking it to act on the rest would produce
/// paths that do not exist. Pure, and tested as such — [`Session::slug_of`] is
/// the way in, the root being what the project says it is.
fn slug_under(root: &Path, worktree: &Path) -> Option<String> {
    let rest = worktree.strip_prefix(root).ok()?;
    let mut parts = rest.components();
    let slug = parts.next()?.as_os_str().to_str()?.to_string();
    parts.next().is_none().then_some(slug)
}

/// The options remembered for a worktree.
///
/// That is what makes a second `wt up` stop asking: a prompt whose name is
/// already here is filtered out, and the previous start is repeated as it was.
/// It is read in the worker and sent back with the questions — the view has no
/// business knowing where `wt` files its state.
pub fn saved_answers(main: &Path, slug: &str) -> BTreeMap<String, String> {
    app(main)
        .map(|app| state::load(&app.root, slug).opts)
        .unwrap_or_default()
}

/// The questions that apply, given the answers already provided.
///
/// Called in a loop by the dialog: a `when` may depend on an earlier answer,
/// and asking every question at once would skip those another unlocks. The
/// loop converges — each round can only add already-answered questions to the
/// list of known ones.
///
/// `task` names the task whose own prompts are wanted, and it is the only case
/// where the list does not come from the phase: `ask = "task"` means "never
/// asked by a phase", so a task's questions are exactly the ones it names.
pub fn questions(
    main: &Path,
    slug: &str,
    answers: &BTreeMap<String, String>,
    phase: Phase,
    task: Option<&str>,
) -> Result<Vec<Question>> {
    let Some(app) = app(main) else {
        return Ok(Vec::new());
    };
    let ask = phase.ask();
    let prompts = match task {
        Some(task) => app
            .task_prompts(task)
            .into_iter()
            // The same filter `prompts_for` applies, and for the same reason:
            // without it the loop would ask the same question for ever.
            .filter(|prompt| prompt.always || !answers.contains_key(&prompt.name))
            .collect(),
        None => app.prompts_for(ask, answers),
    };
    Ok(prompts
        .into_iter()
        .filter(|prompt| app.prompt_applies(prompt, slug, answers, ask))
        .map(|prompt| {
            let choices = app
                .prompt_choices(&prompt, slug, answers, ask)
                .into_iter()
                .map(|option| Choice {
                    label: if option.label.is_empty() {
                        option.value.clone()
                    } else {
                        option.label
                    },
                    value: option.value,
                    detail: option.detail,
                })
                .collect();
            Question {
                title: prompt.title().to_string(),
                name: prompt.name,
                kind: match prompt.kind {
                    PromptKind::Choice => Kind::Choice,
                    PromptKind::Multi => Kind::Multi,
                    PromptKind::Confirm => Kind::Confirm,
                    PromptKind::Text => Kind::Text,
                },
                choices,
                default: prompt.default,
                separator: prompt.separator,
            }
        })
        .collect())
}

/// Creates a worktree with everything the project asks for: the branch
/// following its template, the folders, the copies, the ports, then `post_new`.
///
/// `branch` is an existing branch to check out — or a name imposed on the new
/// one — and `from` where a *new* branch starts; both `None` is what the bare
/// "New worktree" gesture has always meant, a `wt/<slug>` off the main
/// repository's HEAD. The pair is `cmd_new`'s own contract, handed through
/// untouched.
pub fn create(
    main: &Path,
    slug: &str,
    branch: Option<&str>,
    from: Option<&str>,
    answers: &BTreeMap<String, String>,
    progress: Progress,
) -> Result<(PathBuf, String)> {
    let app = app(main).ok_or_else(|| anyhow::anyhow!("this repository has no wt.toml"))?;
    let sets = sets(answers);
    let (output, result) = capturing(&app, Op::New, slug, progress, |app| {
        app.cmd_new(slug, branch, from, &sets)
    });
    result?;
    Ok((app.dir(slug), output))
}

pub fn remove(main: &Path, slug: &str, progress: Progress) -> Result<String> {
    let app = app(main).ok_or_else(|| anyhow::anyhow!("this repository has no wt.toml"))?;
    // `yes`: confirmation is the view's business, and it has already asked.
    let (output, result) = capturing(&app, Op::Remove, slug, progress, |app| {
        app.cmd_rm(slug, true)
    });
    result?;
    Ok(output)
}

/// Starts a worktree, with the answers to the `ask = "up"` questions.
///
/// They are `--set`s, and `wt` merges them into what it had remembered: a start
/// with no answers repeats the previous one, which is exactly what happens when
/// the project asks nothing.
pub fn up(
    main: &Path,
    slug: &str,
    answers: &BTreeMap<String, String>,
    progress: Progress,
) -> Result<String> {
    let app = app(main).ok_or_else(|| anyhow::anyhow!("this repository has no wt.toml"))?;
    let sets = sets(answers);
    let (output, result) = capturing(&app, Op::Up, slug, progress, |app| app.cmd_up(slug, &sets));
    result?;
    Ok(output)
}

pub fn down(main: &Path, slug: &str, progress: Progress) -> Result<String> {
    let app = app(main).ok_or_else(|| anyhow::anyhow!("this repository has no wt.toml"))?;
    let (output, result) = capturing(&app, Op::Down, slug, progress, |app| app.cmd_down(slug));
    result?;
    Ok(output)
}

/// What is needed to launch a task in a terminal.
///
/// The commands are rendered here — templates resolved, environment computed —
/// and not run: the terminal tab launches them, because a task is often
/// interactive and an output pane forwards neither keystrokes nor colours.
pub fn task(
    main: &Path,
    slug: &str,
    name: &str,
    answers: &BTreeMap<String, String>,
) -> Result<Launch> {
    let app = app(main).ok_or_else(|| anyhow::anyhow!("this repository has no wt.toml"))?;
    let task = app
        .project
        .config
        .tasks
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("unknown task: {name}"))?;
    // The answers to the task's own questions become its arguments, split on
    // each prompt's separator: a multiple choice is several arguments, which is
    // what `{{args}}` is interpolated into. Nothing chosen means no argument at
    // all — and a task declared `interactive` then shows its own picker in the
    // terminal tab, which is where it belongs.
    let declared = app.task_prompts(name);
    let args = task_args(
        declared
            .iter()
            .map(|prompt| (prompt.name.as_str(), prompt.separator.as_str())),
        answers,
    );
    let saved = state::load(&app.root, slug);
    let mut vars = app.vars(slug, &saved);
    vars.insert("args".into(), args.join(" "));
    let cwd = match task.cwd {
        ::wt::config::Cwd::Main => app.project.main.clone(),
        ::wt::config::Cwd::Worktree => app.dir(slug),
    };
    Ok(Launch {
        commands: task
            .run
            .0
            .iter()
            .map(|raw| tmpl::render(raw, &vars))
            .collect(),
        env: state::env(&vars),
        cwd,
    })
}

/// What is asked of a `[status] up` probe before it is given up on.
///
/// `wt`'s own runner waits for ever, and this one runs on the single background
/// worker: a `docker compose ps` talking to a daemon that is not answering
/// would take the scan with it — and the summaries and the agents queue behind
/// it. A probe that does not answer is read as "not running", which is the same
/// thing the user sees.
const STATUS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// A project opened once, for the questions asked about each of its worktrees.
///
/// `slug_of`, `is_up` and `endpoints` used to take the repository's path and
/// rebuild the whole thing — a `git rev-parse`, the `wt.toml` parsed, the state
/// read — which the scan paid three times per worktree.
pub struct Session {
    app: App,
}

impl Session {
    /// `None` when the repository has no `wt.toml`, which is the common case.
    pub fn open(main: &Path) -> Option<Self> {
        Some(Self { app: app(main)? })
    }

    /// The slug of a worktree of this project: the directory right under the
    /// root, and nothing deeper.
    pub fn slug_of(&self, worktree: &Path) -> Option<String> {
        slug_under(&self.app.root, worktree)
    }

    /// What `wt` knows of the worktree: running or not, its static address,
    /// and the preview the TUI shows — branch, options, ports, `[status.info]`.
    ///
    /// All of it reads the same saved state, and reading it twice is reading a
    /// file twice for one answer. **`[open] source` is not run here**: the
    /// project's own comment says "resolved on opening, not while listing",
    /// and it used to be run — a `docker exec` and an SQL query per worktree
    /// per scan, with no timeout, on the single background worker — only for
    /// the view to keep the first, static address. See [`Self::links_of`].
    pub fn state_of(&self, slug: &str) -> crate::runtime::protocol::WtWorktree {
        let saved = state::load(&self.app.root, slug);
        let vars = self.app.vars(slug, &saved);
        let env = state::env(&vars);
        let cwd = self.probe_cwd(slug);
        let config = &self.app.project.config;
        let up = config
            .status
            .up
            .as_ref()
            .map(|status| succeeds_within(&tmpl::render(status, &vars), &cwd, &env));
        // The same two guards `App::links` applies to the static address: a
        // template left unresolved is not an address, and a missing label is
        // `wt`'s own default.
        let endpoints = self
            .app
            .url(slug, &saved)
            .filter(|url| !url.contains("{{"))
            .map(|url| Endpoint {
                url,
                label: config
                    .open
                    .label
                    .clone()
                    .unwrap_or_else(|| "application".into()),
            })
            .into_iter()
            .collect();
        // `[status.info]`, each with the probe's ceiling: they run on the same
        // single worker, and one reading a `.env` through a container that
        // does not answer would hold the scan the way the probe did.
        let info = config
            .status
            .info
            .iter()
            .map(|(name, command)| {
                let value = capture_within(&tmpl::render(command, &vars), &cwd, &env);
                (name.clone(), value)
            })
            .collect();
        crate::runtime::protocol::WtWorktree {
            up,
            endpoints,
            branch: saved.branch.clone(),
            opts: saved.opts.clone(),
            ports: saved.ports.clone(),
            info,
        }
    }

    /// The addresses `[open] source` enumerates, and only those.
    ///
    /// Exactly `App::links` minus the static address — one line per link,
    /// `url<TAB>label`, an empty label falling back to the URL — with the
    /// probe's ceiling instead of `util::capture`'s none: it is asked by a
    /// click, and a query that hangs must give the menu back.
    pub fn links_of(&self, slug: &str) -> Vec<Endpoint> {
        let Some(source) = &self.app.project.config.open.source else {
            return Vec::new();
        };
        let saved = state::load(&self.app.root, slug);
        let vars = self.app.vars(slug, &saved);
        let raw = capture_within(
            &tmpl::render(source, &vars),
            &self.probe_cwd(slug),
            &state::env(&vars),
        );
        parse_links(&raw)
    }

    /// Where a worktree's probes run: its directory, or the main repository's
    /// while it does not exist yet — `wt`'s own rule (`prompt_cwd`, which is
    /// private to it).
    fn probe_cwd(&self, slug: &str) -> PathBuf {
        let dir = self.app.dir(slug);
        if dir.is_dir() {
            dir
        } else {
            self.app.project.main.clone()
        }
    }
}

/// What `[open] source` wrote, read the way `wt` reads it: one link per
/// non-empty line, `url<TAB>label`, the label defaulting to the URL.
fn parse_links(raw: &str) -> Vec<Endpoint> {
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let (url, label) = match line.split_once('\t') {
                Some((url, label)) => (url.trim(), label.trim()),
                None => (line.trim(), ""),
            };
            (!url.is_empty()).then(|| Endpoint {
                url: url.to_string(),
                label: if label.is_empty() { url } else { label }.to_string(),
            })
        })
        .collect()
}

/// `util::capture` with a ceiling — see [`STATUS_TIMEOUT`]. What the command
/// wrote on its standard output, trimmed; nothing when it failed to start, ran
/// over, or wrote nothing, which is how `wt`'s own previews read it.
fn capture_within(command: &str, cwd: &Path, env: &BTreeMap<String, String>) -> String {
    match run_within(command, cwd, env) {
        Ok(out) => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        Err(e) => {
            log::warn!("{e:#}");
            String::new()
        }
    }
}

/// `util::succeeds` with a ceiling — see [`STATUS_TIMEOUT`].
fn succeeds_within(command: &str, cwd: &Path, env: &BTreeMap<String, String>) -> bool {
    match run_within(command, cwd, env) {
        Ok(out) => out.status.success(),
        Err(e) => {
            log::warn!("{e:#}");
            false
        }
    }
}

/// A project's shell line, run with the probe's ceiling.
///
/// `WT_SHELL` and its `sh` default are `wt`'s own: hooks are written for a
/// POSIX shell, whatever the user's login shell may be.
fn run_within(
    command: &str,
    cwd: &Path,
    env: &BTreeMap<String, String>,
) -> Result<std::process::Output> {
    let shell = std::env::var("WT_SHELL").unwrap_or_else(|_| "sh".to_string());
    let mut cmd = std::process::Command::new(shell);
    cmd.arg("-c")
        .arg(command)
        .current_dir(cwd)
        .envs(env)
        // The same guard as every git command's: with stdin open, a probe
        // asking anything of the user holds the worker for ever.
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    crate::git::wait_with_timeout(cmd, STATUS_TIMEOUT, || format!("wt status: {command}"))
}

/// A task's arguments, from the answers to the prompts it declares.
///
/// The order is the task's own — the order the values are read in — and not the
/// answers': a hook receiving its arguments in the wrong order would act on the
/// wrong thing without ever saying so.
fn task_args<'a>(
    prompts: impl IntoIterator<Item = (&'a str, &'a str)>,
    answers: &BTreeMap<String, String>,
) -> Vec<String> {
    prompts
        .into_iter()
        .filter_map(|(name, separator)| answers.get(name).map(|value| (separator, value.as_str())))
        .flat_map(|(separator, value)| value.split(separator))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

/// The answers, in the `key=value` form `wt` expects.
fn sets(answers: &BTreeMap<String, String>) -> Vec<String> {
    answers
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect()
}

/// Runs an operation while relaying what it says, and files all of it in the
/// journal.
///
/// `set_sink` exists for that: without it the messages would go to a graphical
/// application's standard output, that is, nowhere. What comes back becomes the
/// notification text — the one `wt` wrote, and not a rewording that would only
/// add approximations.
///
/// **Every line goes out as it is said, and the last one is returned.** A
/// `wt new` narrates its whole sequence — the branch, the folders, the copies,
/// the ports, then whatever `post_new` prints — and that narration is the only
/// account there is of what a project's hooks did. It used to be drained once
/// the operation had returned: a three-minute `composer install` was a spinner,
/// and a failure halfway through left one sentence of error and nothing of the
/// steps that led to it. So the channel is drained **while** the operation
/// runs, by a thread of its own — the operation holds this one — and each line
/// is handed to `progress` the moment it arrives. The last line is still what
/// the caller gets: the balloon shows one, and it is the result one wants to
/// read there, not the first step.
fn capturing<T>(
    app: &App,
    op: Op,
    slug: &str,
    progress: Progress,
    run: impl FnOnce(&App) -> Result<T>,
) -> (String, Result<T>) {
    let what = format!("{} {slug}", op.name());
    let started = std::time::Instant::now();
    log::info!("wt {what}…");
    let (tx, rx) = std::sync::mpsc::channel();
    app.set_sink(Some(tx));
    let (last, result) = std::thread::scope(|scope| {
        // The receiver moves into the drain: `Receiver` is not `Sync`, and it
        // is the drain's alone anyway.
        let what = &what;
        let drain = scope.spawn(move || {
            let mut last = String::new();
            // `recv` and not `try_iter`: the loop ends when the last sender is
            // dropped, which is the `set_sink(None)` below.
            while let Ok(msg) = rx.recv() {
                let (line, warning) = match msg {
                    util::Msg::Warn(m) => (strip_ansi(&m), true),
                    util::Msg::Info(m) | util::Msg::Ok(m) | util::Msg::Out(m) => {
                        (strip_ansi(&m), false)
                    }
                    // `Done` carries no text of its own: it marks the end of a
                    // step.
                    util::Msg::Done(_) => continue,
                };
                if warning {
                    log::warn!("wt {what}: {line}");
                } else {
                    log::info!("wt {what}: {line}");
                }
                last.clone_from(&line);
                progress(line, warning);
            }
            last
        });
        let result = run(app);
        // The sink is released before joining the drain: while it holds the
        // sender, the channel never ends.
        app.set_sink(None);
        let last = drain.join().unwrap_or_default();
        (last, result)
    });
    let elapsed = crate::logging::ms(started.elapsed());
    match &result {
        Ok(_) => log::info!("wt {what} — done in {elapsed}"),
        Err(e) => log::warn!("wt {what} — failed after {elapsed}: {e:#}"),
    }
    (last, result)
}

/// A hook's line, undressed: ANSI escapes are instructions to a terminal, and
/// the console panel and the journal are plain text — an SGR left in shows as
/// `␛[36m` in the middle of the sentence. CSI sequences (colours, cursor),
/// OSC ones (titles, up to BEL or ST) and the two-byte escapes are dropped;
/// the carriage returns of a progress bar go with them.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                // CSI: parameters and intermediates, then one final byte.
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC: swallowed up to BEL or ST (ESC \).
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{07}' || (c == '\u{1b}' && chars.peek() == Some(&'\\')) {
                            if c == '\u{1b}' {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                // Two-byte escapes (ESC c, ESC =, …): the second byte goes too.
                Some(_) | None => {}
            },
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The answers to a task's questions become its arguments — that is how
    /// `db-add` learns which tenants to clone.
    #[test]
    fn a_tasks_answers_become_its_arguments() {
        let answers = BTreeMap::from([
            ("add_tenants".to_string(), "itcs, acme ,".to_string()),
            ("unrelated".to_string(), "ignored".to_string()),
        ]);
        // The order is the **task's**, not the answers': a hook receiving its
        // arguments in the wrong order acts on the wrong thing in silence.
        let args = task_args([("add_tenants", ","), ("mode", ",")], &answers);
        assert_eq!(args, ["itcs", "acme"]);
    }

    /// Nothing chosen means no argument at all, and never an empty one: a task
    /// declared `interactive` then shows its own picker in the terminal, where
    /// an empty string would have been taken for a name.
    #[test]
    fn an_empty_answer_produces_no_argument() {
        let answers = BTreeMap::from([("add_tenants".to_string(), String::new())]);
        assert!(task_args([("add_tenants", ",")], &answers).is_empty());
    }

    /// The dress a hook's line arrives in — colours, a title, a progress
    /// bar's carriage return — is dropped whole; the words stay.
    #[test]
    fn ansi_escapes_are_stripped_and_the_words_stay() {
        assert_eq!(
            strip_ansi("\u{1b}[36mvendor\u{1b}[0m recable\r"),
            "vendor recable"
        );
        assert_eq!(strip_ansi("\u{1b}]0;title\u{07}plain \u{1b}c!"), "plain !");
        assert_eq!(strip_ansi("sans habit"), "sans habit");
    }

    #[test]
    fn a_slug_is_the_directory_right_under_the_root() {
        let root = Path::new("/p/repo-wt");
        // The rule `slug_of` applies once the root is known; the function
        // itself needs a `wt.toml`.
        let check = |path: &str| slug_under(root, Path::new(path));
        assert_eq!(check("/p/repo-wt/demo"), Some("demo".into()));
        // A subdirectory is not a worktree.
        assert_eq!(check("/p/repo-wt/demo/src"), None);
        // The main repository is not under the root: `wt` does not know it.
        assert_eq!(check("/p/repo"), None);
    }

    /// What `[open] source` writes is read as `wt` reads it: a tab splits the
    /// address from its label, a line without one is an address labelled by
    /// itself, and blank lines are nothing.
    #[test]
    fn the_links_a_source_writes_are_read_like_wt_reads_them() {
        let links = parse_links(
            "http://itcs.demo.wt.localhost\titcs\n\n  http://acme.demo.wt.localhost  \n\t\n",
        );
        let pairs: Vec<(&str, &str)> = links
            .iter()
            .map(|link| (link.url.as_str(), link.label.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("http://itcs.demo.wt.localhost", "itcs"),
                (
                    "http://acme.demo.wt.localhost",
                    "http://acme.demo.wt.localhost"
                ),
            ]
        );
    }

    /// The slug suggested for a branch is `wt`'s own, remote prefix dropped:
    /// the command line and the window must agree on the folder a branch gets.
    #[test]
    fn a_branch_suggests_the_slug_wt_would() {
        assert_eq!(
            suggest_slug("origin/feature/Refonte_Devis"),
            "feature-refonte-devis"
        );
        assert_eq!(suggest_slug("fix-42"), "fix-42");
        assert_eq!(suggest_slug(""), "");
    }

    /// The folder rule is read from `wt` and not copied: what the dialog lets
    /// through is what `cmd_new` will accept.
    #[test]
    fn a_slug_is_lowercase_digits_and_inner_dashes() {
        for ok in ["demo", "fix-42", "a1"] {
            assert!(slug_is_valid(ok), "{ok}");
        }
        for bad in ["", "Demo", "-a", "a-", "a b", "a/b", "\u{e9}"] {
            assert!(!slug_is_valid(bad), "{bad}");
        }
    }

    #[test]
    fn the_answers_become_the_sets_wt_expects() {
        let mut answers = BTreeMap::new();
        answers.insert("tenants".to_string(), "a,b".to_string());
        answers.insert("queue".to_string(), "1".to_string());
        assert_eq!(sets(&answers), vec!["queue=1", "tenants=a,b"]);
    }
}
