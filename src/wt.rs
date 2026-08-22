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
/// paths that do not exist.
pub fn slug_of(main: &Path, worktree: &Path) -> Option<String> {
    let root = app(main)?.root;
    let rest = worktree.strip_prefix(&root).ok()?;
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
pub fn create(
    main: &Path,
    slug: &str,
    from: Option<&str>,
    answers: &BTreeMap<String, String>,
) -> Result<(PathBuf, String)> {
    let app = app(main).ok_or_else(|| anyhow::anyhow!("this repository has no wt.toml"))?;
    let sets = sets(answers);
    let (output, result) = capturing(&app, &format!("new {slug}"), |app| {
        app.cmd_new(slug, None, from, &sets)
    });
    result?;
    Ok((app.dir(slug), output))
}

pub fn remove(main: &Path, slug: &str) -> Result<String> {
    let app = app(main).ok_or_else(|| anyhow::anyhow!("this repository has no wt.toml"))?;
    // `yes`: confirmation is the view's business, and it has already asked.
    let (output, result) = capturing(&app, &format!("rm {slug}"), |app| app.cmd_rm(slug, true));
    result?;
    Ok(output)
}

/// Starts a worktree, with the answers to the `ask = "up"` questions.
///
/// They are `--set`s, and `wt` merges them into what it had remembered: a start
/// with no answers repeats the previous one, which is exactly what happens when
/// the project asks nothing.
pub fn up(main: &Path, slug: &str, answers: &BTreeMap<String, String>) -> Result<String> {
    let app = app(main).ok_or_else(|| anyhow::anyhow!("this repository has no wt.toml"))?;
    let sets = sets(answers);
    let (output, result) = capturing(&app, &format!("up {slug}"), |app| app.cmd_up(slug, &sets));
    result?;
    Ok(output)
}

pub fn down(main: &Path, slug: &str) -> Result<String> {
    let app = app(main).ok_or_else(|| anyhow::anyhow!("this repository has no wt.toml"))?;
    let (output, result) = capturing(&app, &format!("down {slug}"), |app| app.cmd_down(slug));
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

/// Is the worktree running, according to `[status] up`?
///
/// `None` when the project does not declare it: there is then nothing to
/// start, and showing "stopped" would be false information.
pub fn is_up(main: &Path, slug: &str) -> Option<bool> {
    let app = app(main)?;
    let status = app.project.config.status.up.as_ref()?;
    let saved = state::load(&app.root, slug);
    let vars = app.vars(slug, &saved);
    Some(util::succeeds(
        &tmpl::render(status, &vars),
        &app.dir(slug),
        &state::env(&vars),
    ))
}

/// The addresses the project exposes for this worktree.
pub fn endpoints(main: &Path, slug: &str) -> Vec<Endpoint> {
    let Some(app) = app(main) else {
        return Vec::new();
    };
    let saved = state::load(&app.root, slug);
    app.links(slug, &saved)
        .into_iter()
        .map(|link: ops::Link| Endpoint {
            url: link.url,
            label: link.label,
        })
        .collect()
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

/// Runs an operation while collecting what it says, and files all of it in the
/// journal.
///
/// `set_sink` exists for that: without it the messages would go to a graphical
/// application's standard output, that is, nowhere. What comes back becomes the
/// notification text — the one `wt` wrote, and not a rewording that would only
/// add approximations.
///
/// **Only the last line is returned, but every one is logged.** A `wt new`
/// narrates its whole sequence — the branch, the folders, the copies, the ports,
/// then whatever `post_new` prints — and that narration is the only account
/// there is of what a project's hooks did. Keeping the last line for the status
/// bar and dropping the rest meant a three-minute `composer install` left one
/// sentence behind it; a failure halfway through left nothing at all, the error
/// being carried by the `Result` and the steps that led to it by nobody.
fn capturing<T>(app: &App, what: &str, run: impl FnOnce(&App) -> Result<T>) -> (String, Result<T>) {
    let started = std::time::Instant::now();
    log::info!("wt {what}…");
    let (tx, rx) = std::sync::mpsc::channel();
    app.set_sink(Some(tx));
    let result = run(app);
    // The sink is released before draining the channel: while it holds the
    // sender, `try_iter` would never see the end.
    app.set_sink(None);
    let mut last = String::new();
    for msg in rx.try_iter() {
        let (line, warning) = match msg {
            util::Msg::Warn(m) => (m, true),
            util::Msg::Info(m) | util::Msg::Ok(m) | util::Msg::Out(m) => (m, false),
            // `Done` carries no text of its own: it marks the end of a step.
            util::Msg::Done(_) => continue,
        };
        if warning {
            log::warn!("wt {what}: {line}");
        } else {
            log::info!("wt {what}: {line}");
        }
        last = line;
    }
    let elapsed = crate::logging::ms(started.elapsed());
    match &result {
        Ok(_) => log::info!("wt {what} — done in {elapsed}"),
        Err(e) => log::warn!("wt {what} — failed after {elapsed}: {e:#}"),
    }
    // The last line only for the caller: the status bar shows just one, and it
    // is the result one wants to read there, not the first step.
    (last, result)
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

    #[test]
    fn a_slug_is_the_directory_right_under_the_root() {
        let root = Path::new("/p/repo-wt");
        // What `slug_of` does once the root is known. The function itself needs
        // a `wt.toml`; it is the rule it applies that matters.
        let check = |path: &str| -> Option<String> {
            let rest = Path::new(path).strip_prefix(root).ok()?;
            let mut parts = rest.components();
            let slug = parts.next()?.as_os_str().to_str()?.to_string();
            parts.next().is_none().then_some(slug)
        };
        assert_eq!(check("/p/repo-wt/demo"), Some("demo".into()));
        // A subdirectory is not a worktree.
        assert_eq!(check("/p/repo-wt/demo/src"), None);
        // The main repository is not under the root: `wt` does not know it.
        assert_eq!(check("/p/repo"), None);
    }

    #[test]
    fn the_answers_become_the_sets_wt_expects() {
        let mut answers = BTreeMap::new();
        answers.insert("tenants".to_string(), "a,b".to_string());
        answers.insert("queue".to_string(), "1".to_string());
        assert_eq!(sets(&answers), vec!["queue=1", "tenants=a,b"]);
    }
}
