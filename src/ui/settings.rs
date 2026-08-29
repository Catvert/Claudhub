//! Persistent settings.
//!
//! A single JSON file in the user's configuration directory. Everything in it is
//! optional: a missing key takes its default value, which makes adding a setting
//! compatible with files already written, and an unreadable file never prevents
//! Claudhub from starting.
//!
//! The settings live in a gpui global rather than in `ClaudhubApp`:
//! gpui-component's form reads and writes each field through closures that only
//! receive an `App`, with no access to the root entity. Going through a global
//! is what makes it possible to write a setting from anywhere — the form, a zoom
//! shortcut, the wheel.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{App, BorrowAppContext};
use serde::{Deserialize, Serialize};

/// Embedded interface font. It is always available, which makes it the only
/// default we can promise.
pub const DEFAULT_UI_FONT: &str = "Iosevka Aile";
/// Embedded fixed-pitch font, the terminal's, the editor's and the diffs'.
pub const DEFAULT_MONO_FONT: &str = "Iosevka Nerd Font Mono";

/// What writes a commit message when nothing is configured.
///
/// `-p` makes `claude` a filter: it reads its prompt from standard input and
/// writes its answer to the output, with no session and no interface. The model
/// is named: a smaller one was tried first and its messages missed the point of
/// a change often enough to need rewriting by hand.
pub const DEFAULT_COMMIT_MESSAGE_COMMAND: &str = "claude -p --model opus";

/// Fallback palettes: the ones gpui-component provides, and which therefore
/// exist even if the theme directory is empty or unreadable.
pub const DEFAULT_LIGHT_THEME: &str = "Default Light";
pub const DEFAULT_DARK_THEME: &str = "Default Dark";

/// Delay before the file is written after a change.
///
/// An input field emits a value per keystroke and the wheel one per notch:
/// writing every time would mean dozens of file opens for one gesture. Half a
/// second is short enough that an abrupt shutdown loses nothing visible.
const SAVE_DELAY: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    Light,
    #[default]
    Dark,
    /// Follows the system's light/dark setting.
    System,
}

impl ThemeMode {
    /// The value as it travels through the form, which only handles strings.
    pub fn as_key(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::System => "system",
        }
    }

    pub fn from_key(key: &str) -> Self {
        match key {
            "light" => Self::Light,
            "system" => Self::System,
            _ => Self::Dark,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LanguageChoice {
    #[default]
    System,
    Fr,
    En,
}

impl LanguageChoice {
    pub fn to_lang_id(self) -> &'static str {
        match self {
            Self::Fr => "fr",
            Self::En => "en",
            Self::System => {
                // `fr-BE`, `fr_FR.UTF-8`: only the language interests us.
                let locale = std::env::var("LC_ALL")
                    .or_else(|_| std::env::var("LC_MESSAGES"))
                    .or_else(|_| std::env::var("LANG"))
                    .unwrap_or_default();
                if locale.starts_with("fr") {
                    "fr"
                } else {
                    "en"
                }
            }
        }
    }

    pub fn as_key(self) -> &'static str {
        match self {
            Self::Fr => "fr",
            Self::En => "en",
            Self::System => "system",
        }
    }

    pub fn from_key(key: &str) -> Self {
        match key {
            "fr" => Self::Fr,
            "en" => Self::En,
            _ => Self::System,
        }
    }
}

/// A coding agent we know how to launch.
///
/// Several profiles and not a single setting: as soon as text is sent to an
/// agent, one wants to choose which. `env` is what carries the model
/// (`ANTHROPIC_MODEL`, one key per profile…) — "configuring several models"
/// therefore calls for no HTTP dependency, only one more variable.
///
/// `command` and `args` are **separate**: splitting a command line on spaces
/// breaks on any path containing one, and that is the kind of failure you only
/// understand after reading the code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AgentProfile {
    /// What the menu shows. Empty, the program's name is used.
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    /// Variables added to the pty's environment.
    ///
    /// `BTreeMap` and not `HashMap`: JSON serialised in a different order on
    /// every write would make a file that changes with nothing having changed.
    pub env: std::collections::BTreeMap<String, String>,
}

impl AgentProfile {
    /// The profile shipped by default, the one that gives the project its name.
    pub fn claude() -> Self {
        Self {
            name: "claude".into(),
            command: "claude".into(),
            args: Vec::new(),
            env: Default::default(),
        }
    }

    /// A profile built from a whole command line.
    pub fn from_command_line(line: &str) -> Option<Self> {
        let mut parts = split_command(line).into_iter();
        let command = parts.next()?;
        let name = command_name(&command).to_string();
        Some(Self {
            name,
            command,
            args: parts.collect(),
            env: Default::default(),
        })
    }

    /// The displayed name: its own, or the program's.
    pub fn label(&self) -> &str {
        non_empty(&self.name, &self.command)
    }

    pub fn spawn(&self) -> (String, Vec<String>) {
        (self.command.clone(), self.args.clone())
    }

    /// The command line as it is typed in, quotes put back.
    pub fn command_line(&self) -> String {
        join_command(
            std::iter::once(self.command.as_str()).chain(self.args.iter().map(String::as_str)),
        )
    }

    /// The environment as it is typed in: `KEY=value`, space-separated, with the
    /// same quoting rules as the command line.
    pub fn env_line(&self) -> String {
        join_command(self.env.iter().map(|(key, value)| format!("{key}={value}")))
    }

    pub fn set_env_line(&mut self, line: &str) {
        self.env = split_command(line)
            .into_iter()
            .filter_map(|pair| {
                let (key, value) = pair.split_once('=')?;
                (!key.is_empty()).then(|| (key.to_string(), value.to_string()))
            })
            .collect();
    }
}

/// A program's name, stripped of its path.
fn command_name(command: &str) -> &str {
    command.rsplit('/').next().unwrap_or(command)
}

// Splitting a command line lives in `crate::cmdline`: the workers use it too,
// and a server binary without gpui has to be able to do it. Re-exported here
// because the form is what uses it most.
pub use crate::cmdline::{join_command, split_command};

/// Where a screen's **first** terminal opens: under the content, or beside it.
///
/// Only the first: the ones that follow join its tab group, and a terminal
/// dragged elsewhere stays where it was put — the layout is saved. A setting
/// because the answer depends on the screen's shape, not on the program: a wide
/// screen has room to the right and none at the bottom, a tall one the reverse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TerminalPlacement {
    #[default]
    Bottom,
    Right,
}

impl TerminalPlacement {
    /// The value as it travels through the form, which only handles strings.
    pub fn as_key(self) -> &'static str {
        match self {
            Self::Bottom => "bottom",
            Self::Right => "right",
        }
    }

    pub fn from_key(key: &str) -> Self {
        match key {
            "right" => Self::Right,
            _ => Self::Bottom,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalSettings {
    /// Program launched in a new tab. Empty = the login shell.
    pub shell: String,
    /// The terminal's font. Empty = the diffs', so that setting the fixed pitch
    /// once is enough for the common case.
    pub font_family: String,
    pub font_size: f32,
    pub scrollback: usize,
    /// An old setting, replaced by `agents`. It is only read once, at migration,
    /// then emptied — but it stays declared, otherwise the file of a user who
    /// has not migrated yet would lose their command.
    pub agent_command: String,
    /// The agents we know how to launch, in menu order.
    pub agents: Vec<AgentProfile>,
    /// Name of the profile launched by default. Empty, or unknown: the first.
    pub default_agent: String,
    /// Which side a screen's first terminal opens on.
    pub placement: TerminalPlacement,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            shell: String::new(),
            font_family: String::new(),
            font_size: 13.0,
            // 10,000 lines: enough to scroll back through a test suite's output
            // without the memory of a dozen tabs being noticeable.
            scrollback: 10_000,
            agent_command: "claude".into(),
            // Empty by default: `migrate_agents` fills it, which makes the same
            // code both the fresh-install path and the path for picking up a
            // file written by an earlier version.
            agents: Vec::new(),
            default_agent: String::new(),
            placement: TerminalPlacement::default(),
        }
    }
}

impl TerminalSettings {
    /// The program to launch, split into command and arguments.
    ///
    /// Returns `None` for an empty setting, which leaves alacritty to open the
    /// login shell as `/etc/passwd` declares it. The setting accepts a whole
    /// command line — `fish -l`, `tmux new-session -A -s claudhub` — because a
    /// bare shell is not always what one wants to open.
    pub fn program(&self) -> Option<(String, Vec<String>)> {
        let mut parts = split_command(&self.shell).into_iter();
        let program = parts.next()?;
        Some((program, parts.collect()))
    }

    /// Picks `agent_command` up as a profile.
    ///
    /// Without risk: `#[serde(default)]` means a file with no `agents` is read
    /// with an empty list, and that is exactly the case caught here. The old
    /// field is emptied so the pick-up only happens once.
    pub fn migrate_agents(&mut self) {
        if !self.agents.is_empty() {
            self.agent_command.clear();
            return;
        }
        self.agents = AgentProfile::from_command_line(&self.agent_command)
            .map(|profile| vec![profile])
            .unwrap_or_else(|| vec![AgentProfile::claude()]);
        self.agent_command.clear();
    }

    /// The profile launched when nobody says which.
    pub fn default_profile(&self) -> Option<&AgentProfile> {
        self.agents
            .iter()
            .find(|profile| profile.label() == self.default_agent)
            .or_else(|| self.agents.first())
    }

    /// The program names to look for in `/proc`.
    ///
    /// Every profile and not only the current one: an agent launched from a
    /// terminal alongside counts as much as the one started here, and looking
    /// for only one would see only half of them.
    pub fn agent_programs(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .agents
            .iter()
            .map(|profile| command_name(&profile.command).to_string())
            .filter(|name| !name.is_empty())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// The effective font: its own, or the diffs'.
    pub fn family<'a>(&'a self, mono: &'a str) -> &'a str {
        if self.font_family.is_empty() {
            mono
        } else {
            &self.font_family
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeMode,
    /// Named palettes, one per appearance. Two settings and not one: `theme`
    /// says whether it is light or dark — the system may decide — and these say
    /// *which* palette carries each of the two appearances.
    pub light_theme: String,
    pub dark_theme: String,
    pub language: LanguageChoice,
    /// The interface font: labels, menus, lists.
    pub ui_font_family: String,
    /// The fixed-pitch font: diffs, shas, aligned paths.
    pub mono_font_family: String,
    pub font_size: f32,
    /// The diffs' text size, independent of the interface's: one enlarges code
    /// to read it without wanting to enlarge the whole window.
    pub diff_font_size: f32,
    pub start_maximized: bool,
    pub terminal: TerminalSettings,
    /// Repositories reopened at startup, in the order they were opened.
    pub repositories: Vec<PathBuf>,
    /// Number of context lines around a hunk in the review.
    pub diff_context: usize,
    /// The commit graph drawn left of the history rows. Off by default: its
    /// gutter is sized by the widest point of the whole graph, and the width
    /// it reserves comes off the summary — the column a history is read by.
    pub history_graph: bool,
    /// The branches, in a column left of the history. On by default: the panel
    /// only offers it where it is wide enough to hold three columns, and there
    /// the branch one reads the log of is the thing one reaches for first.
    pub history_branches: bool,
    /// The file list as a folder tree rather than flat. Flat by default: a
    /// change set is a handful of files, and the folder rows were mostly
    /// chrome between them.
    pub review_tree: bool,
    /// Diff in two columns — old version on the left, new on the right — rather
    /// than as a single list.
    ///
    /// True by default: this is the reading a review asks for, and it is the only
    /// view where `diff_wrap` applies.
    pub diff_split: bool,
    /// Show the whole file around the changes, and not only their few lines of
    /// context.
    pub diff_whole_file: bool,
    /// Wrap long lines, in two-column view.
    ///
    /// True by default, and this is where it counts: a column is half the view,
    /// and the slightest long line forced horizontal scrolling of the whole file
    /// — the width being that of the longest line, one was enough to give it to
    /// all. In a single column the line has the whole width and wrapping is
    /// debatable; so it does not happen.
    pub diff_wrap: bool,
    /// "Update from base" replays the branch instead of merging.
    ///
    /// Both are defensible and the choice is a team habit: a linear history on
    /// one side, the trace of what was integrated and when on the other. Merge by
    /// default, because it rewrites nothing and therefore never breaks a branch
    /// already pushed.
    pub update_with_rebase: bool,
    /// Integrating forces a merge commit, even when a fast-forward would be
    /// possible: that is what keeps a trace of the agent's branch.
    pub integrate_no_ff: bool,
    /// The external editor's command, with `{path}` and `{line}`.
    ///
    /// Editing in Claudhub stays light: a short touch-up here, the real work in
    /// the editor of your choice. Empty, the gesture disappears from the menus
    /// rather than failing.
    pub external_editor: String,
    /// Also show the files `.gitignore` leaves out, in the explorer.
    pub show_ignored_files: bool,
    /// How many file tabs one worktree keeps at once. `0`: no limit.
    ///
    /// Ten, as PhpStorm's own: browsing a project opens far more files than one
    /// reads, and a bar nobody prunes is a bar one stops reading. What closes is
    /// the tab read longest ago, and never one holding unsaved text — losing
    /// work to make room for a glance is the one thing the limit must not do.
    pub editor_tab_limit: usize,
    /// `Ctrl+S` writes every unsaved tab, and not only the one on screen.
    ///
    /// On by default: one edits a file, follows a call into another, edits that
    /// one too, and the gesture at the end of it means "put my work on disk".
    /// It is what an IDE does — PhpStorm has no per-file save at all — and what
    /// it removes is the tab left unsaved behind another, found by a build.
    pub save_all_tabs: bool,
    /// One clicked file at a time reuses the same tab, until it is kept.
    ///
    /// VS Code's preview tab: a single click in the tree shows the file in a
    /// tab that the next click replaces, and typing in it — or opening it with
    /// Enter, or double-clicking — keeps it for good. It is the answer to
    /// browsing, where the limit is the answer to a long session.
    pub editor_preview_tab: bool,
    /// Minutes between two automatic `git fetch`es. `0`: none.
    ///
    /// Without it, "three commits behind" only appears after a fetch asked for
    /// by hand — that is, once one already suspected something. A regular
    /// reading is what makes the button's count trustworthy; it is what desktop
    /// git clients do, and they all have a setting for it.
    pub auto_fetch_minutes: u32,
    /// What writes a commit message from the staged diff.
    ///
    /// A program and not an API: that is Claudhub's framing decision, and it
    /// holds here as for the terminal's agents. The diff reaches it on standard
    /// input, its standard output is the message. `opus` by default: a faster
    /// model wrote messages that missed the point of the change.
    /// Empty, the button disappears.
    pub commit_message_command: String,
    /// The Sentry organisation. The *project*, for its part, belongs to the
    /// repository and lives in the state store, not here: five checkouts of one
    /// code have the same errors, and two repositories of one organisation do
    /// not.
    pub sentry_org: String,
    /// The instance. Empty is `sentry.io`; a self-hosted one says so here,
    /// which is the only thing that changes about it.
    pub sentry_host: String,
    /// API token.
    ///
    /// Written `$SENTRY_TOKEN`, it is read from the **worker's** environment
    /// and never stored: this file is 0600, which does not make it a vault, and
    /// a token lying around in a configuration backup is a token that leaks.
    /// See `outside::from_env`.
    pub sentry_token: String,
    /// The query sent to Sentry. Empty: the unresolved issues.
    pub sentry_query: String,
    /// What is said above an error handed to an agent. Empty: our own sentence.
    ///
    /// A setting because it is the one part of that prompt that is about the
    /// project rather than about the error — "this is a Laravel application",
    /// "answer in French" — and everything else in it comes from Sentry.
    pub sentry_intro: String,
    /// Browse diffs and the tree with vim's keys.
    ///
    /// Off by default, and it has to stay that way: those bindings are **bare
    /// letters**, and they take the place of everything a letter could do
    /// elsewhere. It is not an editing mode — there is nothing to edit in a diff
    /// — but the left hand on the home row for reviewing.
    pub vim_mode: bool,
    /// Yank and paste through the **system** clipboard rather than vim's own
    /// register.
    ///
    /// Off by default, which is vim's own default: `y` there fills a register
    /// nothing else can see, and someone who wants `y` here and `Ctrl+V` in the
    /// browser is asking for `unnamedplus`. It is a setting and not a decision
    /// because both answers are right — one keeps the clipboard for what one put
    /// in it deliberately, the other saves a gesture.
    #[serde(default)]
    pub vim_clipboard: bool,
    /// Where the review notes are written, one Markdown file per note.
    ///
    /// Empty: `<config>/notes`. The field exists to point at a vault — Obsidian,
    /// or any folder you already keep — because a review note is text that gets
    /// written, and it has no business being locked in a state JSON nothing else
    /// can read.
    pub notes_dir: String,
    /// The WSL distribution the workers run in, on Windows.
    ///
    /// Empty: it has not been chosen yet, and the interface asks for it on first
    /// startup. A setting and not a detection every time: a machine often has
    /// several distributions, and it is not a question to ask again on every
    /// opening. No effect outside Windows, where the workers are in the same
    /// process.
    pub wsl_distro: String,
    /// Views the user has hidden, by dock panel name.
    ///
    /// Here and not in `layout.json`: it is a choice made once — I do not use
    /// Sentry, I do not want the branches — not the geometry of a window.
    /// `LAYOUT_VERSION` throws the layout away as soon as the panels change
    /// name, and that choice has no business disappearing with it.
    ///
    /// A list of names rather than a boolean per panel: an added panel is
    /// visible without this file having to know about it.
    pub hidden_panels: Vec<String>,
    /// The views **folded away**, which is not the same list.
    ///
    /// Pressing a lit rail button puts a view away for now; the list above is
    /// the decision one makes once. They were one set, and it made the two
    /// gestures undo each other — the button one pressed to get the room back
    /// was the button that then had to disappear.
    pub folded_panels: Vec<String>,
    /// The "Databases" panel's databases.
    ///
    /// Declared here like the agent profiles, and for the same reason: it is the
    /// second level of the extension system — a declaration, not code. A
    /// connection does not belong to a repository: one reviews a project in
    /// several worktrees and the development database is the same.
    pub databases: Vec<crate::db::Connection>,
    /// The keyboard shortcuts the user has changed: binding id → keystrokes.
    ///
    /// Only what differs from ours — the table stays the reference, and a
    /// version of Claudhub that adds a shortcut must not need this file to say
    /// anything about it. An empty value switches a binding **off**: the line
    /// stays, so one sees what one has turned off. See `ui::shortcuts`.
    pub shortcuts: std::collections::BTreeMap<String, String>,
    /// The language servers the editor may start.
    ///
    /// Declared here like the agent profiles and the database connections: the
    /// second level of the extension system, a declaration and not code. The
    /// first level is the project's own `wt.toml`, whose `[lsp.<name>]` says
    /// the same thing in the same shape and **wins** for a language both
    /// declare — the project knows better than the machine what its code wants.
    ///
    /// PHPantom ships as the default entry, not because PHP is special but
    /// because an empty list makes a feature that has to be discovered before
    /// it can be tried. Emptying it is a choice, and it is kept.
    pub lsp: Vec<crate::lsp::Server>,
    /// Rows returned per page in the SQL console.
    ///
    /// One page and not the whole result: a `SELECT *` on a two-million-row
    /// table would fill the window's memory before showing anything at all.
    pub db_page_size: usize,
    /// **Legacy**: what a plugin was told, read once so `migrate_sentry` can
    /// carry Sentry's back into the fields above, then dropped. Nothing writes
    /// it any more.
    #[serde(default)]
    pub plugins: std::collections::BTreeMap<String, PluginSettings>,
}

/// **Legacy**: what one plugin was told, kept only to be read back once.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PluginSettings {
    pub settings: std::collections::BTreeMap<String, String>,
    pub secrets: std::collections::BTreeMap<String, String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeMode::default(),
            light_theme: DEFAULT_LIGHT_THEME.into(),
            dark_theme: DEFAULT_DARK_THEME.into(),
            language: LanguageChoice::default(),
            ui_font_family: DEFAULT_UI_FONT.into(),
            mono_font_family: DEFAULT_MONO_FONT.into(),
            font_size: 14.0,
            diff_font_size: 13.0,
            start_maximized: false,
            terminal: TerminalSettings::default(),
            repositories: Vec::new(),
            diff_context: 3,
            history_graph: false,
            history_branches: true,
            review_tree: false,
            diff_split: true,
            diff_whole_file: false,
            diff_wrap: true,
            update_with_rebase: false,
            integrate_no_ff: true,
            external_editor: String::new(),
            show_ignored_files: true,
            editor_tab_limit: 10,
            editor_preview_tab: true,
            save_all_tabs: true,
            auto_fetch_minutes: 10,
            commit_message_command: DEFAULT_COMMIT_MESSAGE_COMMAND.into(),
            sentry_org: String::new(),
            sentry_host: String::new(),
            sentry_token: String::new(),
            sentry_query: String::new(),
            sentry_intro: String::new(),
            vim_mode: false,
            vim_clipboard: false,
            notes_dir: String::new(),
            wsl_distro: String::new(),
            hidden_panels: Vec::new(),
            folded_panels: Vec::new(),
            databases: Vec::new(),
            lsp: shipped_servers(),
            plugins: Default::default(),
            shortcuts: std::collections::BTreeMap::new(),
            db_page_size: 500,
        }
    }
}

/// The language servers shipped as defaults, and why these three.
///
/// They are the languages this repository and the projects it reviews are
/// written in — PHP with its Blade views, Rust, TypeScript and what comes with
/// it — and none of them is special beyond that. The list is a declaration like
/// any other: emptying it or adding to it is a choice, and it is kept.
///
/// **No binary is downloaded.** Each is a program the user installs, like the
/// terminal's agents and the commit-message command. Absent, the server fails
/// to start and the button says so, which is a better answer than an interface
/// that quietly fetches a hundred megabytes.
fn shipped_servers() -> Vec<crate::lsp::Server> {
    vec![phpantom(), rust_analyzer(), typescript()]
}

/// PHPantom, on PHP and on Blade.
///
/// Blade is served by the same one — it preprocesses a view into virtual PHP —
/// so the two extensions belong to a single entry, and `blade.php` being the
/// longer is what makes it win the tie over `php`.
fn phpantom() -> crate::lsp::Server {
    crate::lsp::Server {
        name: "PHPantom".into(),
        command: "phpantom_lsp".into(),
        args: Vec::new(),
        env: std::collections::BTreeMap::new(),
        extensions: vec!["php".into(), "blade.php".into()],
        language_id: "php".into(),
    }
}

/// rust-analyzer, on Rust.
///
/// No `language_id`: `rs` is one of the extensions the table names, and writing
/// `rust` here would only be a second place to keep it right.
fn rust_analyzer() -> crate::lsp::Server {
    crate::lsp::Server {
        name: "rust-analyzer".into(),
        command: "rust-analyzer".into(),
        args: Vec::new(),
        env: std::collections::BTreeMap::new(),
        extensions: vec!["rs".into()],
        language_id: String::new(),
    }
}

/// typescript-language-server, on TypeScript and JavaScript.
///
/// The four extensions are **four languages** to the protocol — `typescript`,
/// `typescriptreact`, `javascript`, `javascriptreact` — which is exactly why
/// `language_id` is left empty here: a single one written on the entry would be
/// announced for all four, and this server acts on what it is told a document
/// is. `lsp::language_of` gives each file its own.
///
/// `--stdio` is not optional: without it the binary picks a socket and nothing
/// ever answers.
fn typescript() -> crate::lsp::Server {
    crate::lsp::Server {
        name: "TypeScript".into(),
        command: "typescript-language-server".into(),
        args: vec!["--stdio".into()],
        env: std::collections::BTreeMap::new(),
        extensions: vec!["ts".into(), "tsx".into(), "js".into(), "jsx".into()],
        language_id: String::new(),
    }
}

impl Settings {
    /// The delay between two automatic fetches, or `None` when there is none.
    pub fn auto_fetch_period(&self) -> Option<std::time::Duration> {
        (self.auto_fetch_minutes > 0)
            .then(|| std::time::Duration::from_secs(u64::from(self.auto_fetch_minutes) * 60))
    }

    /// The notes root, `~` expanded.
    ///
    /// A path typed into a form is written `~/Vault/…` — that is how it is given
    /// to a shell — and passing it as it is to `std::fs` would create a folder
    /// named `~` in the current directory.
    pub fn notes_root(&self) -> Option<PathBuf> {
        let text = self.notes_dir.trim();
        if text.is_empty() {
            return config_dir().map(|dir| dir.join("notes"));
        }
        match text.strip_prefix("~/") {
            Some(rest) => directories::UserDirs::new().map(|dirs| dirs.home_dir().join(rest)),
            None => Some(PathBuf::from(text)),
        }
    }
}

/// The context asked of git for "the whole file".
///
/// `git diff` has no "whole file" option: we ask it for a context larger than
/// any file, which it brings back to what exists by itself.
pub const WHOLE_FILE_CONTEXT: usize = 1_000_000;

impl Settings {
    /// Context lines to ask for on the displayed file.
    pub fn context_lines(&self) -> usize {
        if self.diff_whole_file {
            WHOLE_FILE_CONTEXT
        } else {
            self.diff_context
        }
    }

    pub fn load() -> Self {
        let mut settings = Self::read();
        settings.terminal.migrate_agents();
        settings.migrate_sentry();
        settings
    }

    /// The Sentry settings a plugin was holding become ours again.
    ///
    /// The way back from the trip through the plugin system, taken the same way
    /// out and in: the values are poured into the fields and the entry is
    /// dropped, so it happens once. What is already set here wins — a window
    /// configured since keeps what it has.
    fn migrate_sentry(&mut self) {
        let Some(plugin) = self.plugins.remove("sentry") else {
            return;
        };
        let carry =
            |field: &mut String, key: &str, from: &std::collections::BTreeMap<String, String>| {
                if !field.trim().is_empty() {
                    return;
                }
                if let Some(value) = from.get(key).filter(|value| !value.trim().is_empty()) {
                    *field = value.clone();
                }
            };
        carry(&mut self.sentry_org, "org", &plugin.settings);
        carry(&mut self.sentry_host, "host", &plugin.settings);
        carry(&mut self.sentry_query, "query", &plugin.settings);
        carry(&mut self.sentry_intro, "intro", &plugin.settings);
        carry(&mut self.sentry_token, "token", &plugin.secrets);
        log::info!(target: "sentry", "settings carried back from the plugin");
    }

    fn read() -> Self {
        let Some(path) = settings_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                // Overwriting a file we failed to read would lose settings over
                // a single malformed key: we start again from the default values
                // without erasing anything.
                log::warn!("unreadable settings ({}): {e}", path.display());
                Self::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                log::warn!("reading the settings: {e}");
                Self::default()
            }
        }
    }

    pub fn save(&self) {
        let Some(path) = settings_path() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = write_private(&path, &json) {
                    log::warn!("writing the settings: {e}");
                }
            }
            Err(e) => log::warn!("serialising the settings: {e}"),
        }
    }

    /// Adds a repository to the list reopened at startup, without duplicates and
    /// at the head: the last opened is the first to be reopened.
    pub fn remember_repository(&mut self, main: &Path) {
        self.repositories.retain(|p| p != main);
        self.repositories.insert(0, main.to_path_buf());
        self.repositories.truncate(20);
    }

    pub fn forget_repository(&mut self, main: &Path) {
        self.repositories.retain(|p| p != main);
    }

    /// The interface's effective font, never empty.
    pub fn ui_font(&self) -> &str {
        non_empty(&self.ui_font_family, DEFAULT_UI_FONT)
    }

    /// The effective fixed-pitch font, never empty.
    pub fn mono_font(&self) -> &str {
        non_empty(&self.mono_font_family, DEFAULT_MONO_FONT)
    }

    /// Police effective du terminal.
    pub fn terminal_font(&self) -> &str {
        self.terminal.family(self.mono_font())
    }
}

fn non_empty<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.trim().is_empty() {
        fallback
    } else {
        value
    }
}

/// An area whose text size is set independently.
///
/// Two areas and not one: enlarging an agent's output to read it must not move
/// the code being reviewed beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zoom {
    Diff,
    Terminal,
}

/// Below eight points the text is no longer readable, above thirty-two a single
/// diff line fills the view: those are the two ways of making the interface
/// unusable, and the wheel gets there fast.
pub const MIN_FONT_SIZE: f32 = 8.0;
pub const MAX_FONT_SIZE: f32 = 32.0;

pub fn clamp_font_size(value: f32) -> f32 {
    value.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)
}

impl Settings {
    fn size_of(&mut self, zone: Zoom) -> &mut f32 {
        match zone {
            Zoom::Diff => &mut self.diff_font_size,
            Zoom::Terminal => &mut self.terminal.font_size,
        }
    }

    /// Adds `steps` points to an area's size. Returns true if something changed
    /// — at the end of the travel, there is nothing to redraw.
    pub fn zoom(&mut self, zone: Zoom, steps: f32) -> bool {
        let size = self.size_of(zone);
        let next = clamp_font_size(*size + steps);
        let changed = next != *size;
        *size = next;
        changed
    }

    pub fn reset_zoom(&mut self, zone: Zoom) -> bool {
        let default = Settings::default();
        let target = match zone {
            Zoom::Diff => default.diff_font_size,
            Zoom::Terminal => default.terminal.font_size,
        };
        let size = self.size_of(zone);
        let changed = *size != target;
        *size = target;
        changed
    }
}

// --- Global -----------------------------------------------------------------

pub struct SettingsStore {
    settings: Settings,
    /// True when a deferred write is already scheduled: later changes join it
    /// instead of scheduling another.
    saving: bool,
}

impl gpui::Global for SettingsStore {}

impl Settings {
    /// Installs the loaded settings. To be called once, at startup.
    pub fn init_global(self, cx: &mut App) {
        cx.set_global(SettingsStore {
            settings: self,
            saving: false,
        });
    }

    pub fn global(cx: &App) -> &Settings {
        &cx.global::<SettingsStore>().settings
    }

    /// Changes the settings, applies what shows at once and schedules the write.
    ///
    /// Only one copy is taken — the state before — and the comparison is made
    /// against the settings in place: this runs on every wheel notch of the zoom
    /// and on every keystroke of the form, and `theme::apply` is expensive
    /// (two `Theme::change`, the tokens recomputed, every window refreshed). It
    /// is therefore called only when a field it reads has moved; the rest of a
    /// change shows through a plain refresh.
    pub fn update_global(cx: &mut App, f: impl FnOnce(&mut Settings)) {
        let before = Self::global(cx).clone();
        cx.update_global::<SettingsStore, _>(|store, _| f(&mut store.settings));
        let after = Self::global(cx);
        if *after == before {
            return;
        }
        let language = after.language;
        let language_changed = language != before.language;
        let theme_changed = affects_theme(&before, after);
        if language_changed {
            crate::ui::set_language(language);
        }
        if theme_changed {
            let after = Self::global(cx).clone();
            crate::ui::theme::apply(&after, None, cx);
        } else {
            cx.refresh_windows();
        }
        schedule_save(cx);
    }
}

/// Whether a change moves one of the fields `theme::apply` reads.
///
/// Kept next to it: adding a setting that `apply` looks at without listing it
/// here would leave the window painted with the old one, and nothing would say
/// so.
fn affects_theme(before: &Settings, after: &Settings) -> bool {
    before.theme != after.theme
        || before.light_theme != after.light_theme
        || before.dark_theme != after.dark_theme
        || before.font_size != after.font_size
        || before.ui_font() != after.ui_font()
        || before.mono_font() != after.mono_font()
}

impl Settings {
    /// Writes now what the deferred save still owes: the application is
    /// quitting, and the half-second timer will not live to fire. A no-op
    /// when nothing is pending. The store's `flush` twin.
    pub fn flush(cx: &mut App) {
        let pending = cx.update_global::<SettingsStore, _>(|store, _| {
            std::mem::replace(&mut store.saving, false).then(|| store.settings.clone())
        });
        if let Some(settings) = pending {
            settings.save();
        }
    }
}

fn schedule_save(cx: &mut App) {
    let already_scheduled =
        cx.update_global::<SettingsStore, _>(|store, _| std::mem::replace(&mut store.saving, true));
    if already_scheduled {
        return;
    }
    cx.spawn(async move |cx| {
        cx.background_executor().timer(SAVE_DELAY).await;
        let settings = cx.update(|cx| {
            cx.update_global::<SettingsStore, _>(|store, _| {
                store.saving = false;
                store.settings.clone()
            })
        });
        settings.save();
    })
    .detach();
}

// --- Choices offered in the form --------------------------------------------

/// Name fragments that give away a fixed-pitch font.
///
/// There is no portable way to query that property in gpui: we fall back on the
/// naming convention, which covers the families a developer will have installed.
/// The list stays editable by hand in the settings file for the cases it misses.
const MONO_HINTS: [&str; 14] = [
    "mono",
    "code",
    "consol",
    "courier",
    "menlo",
    "hack",
    "iosevka",
    "terminus",
    "inconsolata",
    "monaco",
    "andale",
    "jetbrains",
    "cascadia",
    "monospace",
];

pub fn is_monospace_name(name: &str) -> bool {
    let name = name.to_lowercase();
    MONO_HINTS.iter().any(|hint| name.contains(hint))
}

/// Families offered for a font field.
///
/// The embedded font is always present, even if the system does not know it: it
/// is the one used as a fallback, and a choice that cannot be taken back after
/// leaving it would be a trap.
pub fn font_choices(installed: &[String], monospace_only: bool, embedded: &str) -> Vec<String> {
    let mut names: Vec<String> = installed
        .iter()
        // Icon fonts and private variants start with a dot on macOS; they have
        // no business in a list of choices.
        .filter(|name| !name.starts_with('.'))
        .filter(|name| !monospace_only || is_monospace_name(name))
        .cloned()
        .collect();
    names.push(embedded.to_string());
    names.sort_by_key(|name| name.to_lowercase());
    names.dedup_by_key(|name| name.to_lowercase());
    names
}

/// Shells declared by the system, as `/etc/shells` lists them.
pub fn parse_shells(text: &str) -> Vec<String> {
    let mut shells: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with('/'))
        .map(str::to_string)
        .collect();
    shells.dedup();
    shells
}

/// The shells the remote server announced in its handshake.
///
/// A static, and not a field of `ClaudhubApp`: the form declares each field
/// through closures that only receive an `App`, with no access to the root
/// entity — that is already what put the settings in a global. Empty in local
/// mode, where `/etc/shells` is the right file.
static SERVER_SHELLS: std::sync::RwLock<Vec<String>> = std::sync::RwLock::new(Vec::new());

/// Records what the server has just announced. Called on every handshake,
/// reconnections included: one sometimes changes distribution in between.
pub fn set_server_shells(shells: Vec<String>) {
    if let Ok(mut current) = SERVER_SHELLS.write() {
        *current = shells;
    }
}

/// The user's login shell inside the distribution, as `/etc/passwd` declares it
/// over there.
///
/// Recorded at the moment the server is installed, because that is the only
/// moment it can be asked for: a terminal opened by `wsl.exe --exec` has no
/// shell to query `$SHELL` from, and a shell is precisely what we want to launch
/// there.
static SERVER_SHELL: std::sync::RwLock<String> = std::sync::RwLock::new(String::new());

pub fn set_server_shell(shell: String) {
    if let Ok(mut current) = SERVER_SHELL.write() {
        *current = shell;
    }
}

pub fn server_shell() -> Option<String> {
    SERVER_SHELL
        .read()
        .ok()
        .map(|shell| shell.clone())
        .filter(|shell| !shell.is_empty())
}

/// The shells offered: those of the system that still exist on disk.
///
/// The **server's** when there is one: it is its machine that will launch the
/// shell, and ours does not have the same — on Windows it does not even have an
/// `/etc/shells`. They arrive already filtered by their existence over there,
/// which we could not check from here.
pub fn available_shells() -> Vec<String> {
    if let Ok(shells) = SERVER_SHELLS.read() {
        if !shells.is_empty() {
            return shells.clone();
        }
    }
    let text = std::fs::read_to_string("/etc/shells").unwrap_or_default();
    parse_shells(&text)
        .into_iter()
        .filter(|shell| Path::new(shell).exists())
        .collect()
}

/// Where the panel layout is saved.
///
/// Separate from the settings: it is not a preference written by hand but a
/// window's state, bulky and unreadable, and a user opening `settings.json` has
/// no business finding it there.
/// The directory where Claudhub files what it remembers: settings, panel
/// layout, themes.
///
/// Resolved once: the answer never changes while the process runs, and this is
/// read per frame (the notes directory) and per changed path.
pub fn config_dir() -> Option<PathBuf> {
    static DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        directories::ProjectDirs::from("be", "acetics", "claudhub")
            .map(|dirs| dirs.config_dir().to_path_buf())
    })
    .clone()
}

pub fn layout_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("layout.json"))
}

fn settings_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("settings.json"))
}

/// Picks up the configuration written under the project's former name.
///
/// Perch became Claudhub, and the configuration directory carries the project's
/// name: without a pick-up, a user would find a brand-new window at launch,
/// their repositories and their layout forgotten. The move happens once only —
/// if a directory under the new name already exists, either the pick-up has
/// happened or the user started with Claudhub.
///
/// To be called before the first read of the settings. This code only has a
/// reason to exist until the existing installations have moved over.
pub fn migrate_from_perch() {
    let (Some(new), Some(old)) = (
        config_dir(),
        directories::ProjectDirs::from("be", "acetics", "perch")
            .map(|d| d.config_dir().to_path_buf()),
    ) else {
        return;
    };
    if new == old || new.exists() || !old.exists() {
        return;
    }
    if let Err(e) = std::fs::rename(&old, &new) {
        log::warn!("the former configuration could not be taken over: {e}");
        return;
    }
    // The layout names its panels, and those names changed with the project:
    // without this rewrite, the dock would find none of its panels and would
    // start again from the default layout.
    if let Some(path) = layout_path() {
        if let Ok(text) = std::fs::read_to_string(&path) {
            let _ = std::fs::write(&path, text.replace("Perch", "Claudhub"));
        }
    }
    // The bundled themes are rewritten under their new name at startup: the old
    // ones would be duplicates in the list, with the same names.
    if let Ok(entries) = std::fs::read_dir(new.join("themes")) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with("perch-") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    log::info!("configuration taken over from {}", old.display());
}

/// Written 0600 on Unix: the file carries the repositories' paths and the
/// agent's command line, which are none of the other accounts' business on the
/// machine.
///
/// Shared with the state store (`store.rs`), which carries review notes and has
/// no more reason to be readable by everybody.
#[cfg(unix)]
pub(super) fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    // `mode` only applies at creation: a file written by an earlier version
    // would keep its original permissions.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(contents.as_bytes())
}

#[cfg(not(unix))]
pub(super) fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    std::fs::write(path, contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_fields_the_theme_reads_make_it_apply() {
        let base = Settings::default();
        let mut other = base.clone();
        other.diff_context += 1;
        other.show_ignored_files = !other.show_ignored_files;
        assert!(!affects_theme(&base, &other));

        let mut zoomed = base.clone();
        zoomed.font_size += 1.;
        assert!(affects_theme(&base, &zoomed));

        // An empty family falls back on the default: the effective font has not
        // moved, so neither has the theme.
        let mut blank = base.clone();
        blank.ui_font_family = String::new();
        assert_eq!(
            affects_theme(&base, &blank),
            base.ui_font() != blank.ui_font()
        );

        let mut font = base.clone();
        font.mono_font_family = "Nonesuch Mono".into();
        assert!(affects_theme(&base, &font));

        let mut palette = base.clone();
        palette.dark_theme = "whatever".into();
        assert!(affects_theme(&base, &palette));
    }

    /// A shipped entry that serves nothing, or that no file reaches, is a line
    /// in a form that never does anything — and nothing else would say so.
    #[test]
    fn every_shipped_server_serves_the_files_it_names() {
        let servers = shipped_servers();
        for server in &servers {
            assert!(server.is_runnable(), "{} has no command", server.name);
            assert!(
                !server.extensions.is_empty(),
                "{} serves nothing",
                server.name
            );
        }
        let picked = |file: &str| {
            crate::lsp::pick(&servers, std::path::Path::new(file))
                .map(|s| (s.name.clone(), s.language_for(std::path::Path::new(file))))
        };
        assert_eq!(
            picked("a.php"),
            Some(("PHPantom".into(), "php".into())),
            "PHP"
        );
        assert_eq!(
            picked("page.blade.php"),
            Some(("PHPantom".into(), "php".into())),
            "a Blade view goes to the same one"
        );
        assert_eq!(
            picked("main.rs"),
            Some(("rust-analyzer".into(), "rust".into())),
            "Rust"
        );
        assert_eq!(
            picked("app.tsx"),
            Some(("TypeScript".into(), "typescriptreact".into())),
            "TSX is its own language"
        );
        assert_eq!(picked("README.md"), None, "nothing shipped serves Markdown");
    }

    #[test]
    fn missing_keys_take_their_defaults() {
        // A file written by a version that knew nothing of `terminal` must keep
        // loading.
        let s: Settings = serde_json::from_str(r#"{"theme":"light"}"#).unwrap();
        assert_eq!(s.theme, ThemeMode::Light);
        assert_eq!(s.terminal.scrollback, 10_000);
        assert_eq!(s.diff_context, 3);
        // The fields added afterwards too: that is the case for every file
        // already written on a user's disk.
        assert_eq!(s.ui_font_family, DEFAULT_UI_FONT);
        assert_eq!(s.mono_font_family, DEFAULT_MONO_FONT);
    }

    #[test]
    fn recent_repositories_stay_unique_and_ordered() {
        let mut s = Settings::default();
        s.remember_repository(Path::new("/a"));
        s.remember_repository(Path::new("/b"));
        s.remember_repository(Path::new("/a"));
        assert_eq!(
            s.repositories,
            vec![PathBuf::from("/a"), PathBuf::from("/b")]
        );
        s.forget_repository(Path::new("/a"));
        assert_eq!(s.repositories, vec![PathBuf::from("/b")]);
    }

    #[test]
    fn system_language_follows_the_environment() {
        // The exact value depends on the test environment; only the shape
        // matters: two letters the catalogue knows.
        assert!(matches!(LanguageChoice::System.to_lang_id(), "fr" | "en"));
        assert_eq!(LanguageChoice::Fr.to_lang_id(), "fr");
    }

    #[test]
    fn an_emptied_font_falls_back_instead_of_disappearing() {
        // Clearing the field in the form must not make the text invisible: it is
        // the natural gesture before typing something else.
        let mut s = Settings {
            ui_font_family: "  ".into(),
            mono_font_family: String::new(),
            ..Default::default()
        };
        assert_eq!(s.ui_font(), DEFAULT_UI_FONT);
        assert_eq!(s.mono_font(), DEFAULT_MONO_FONT);
        // The terminal with no font of its own follows the diffs'.
        assert_eq!(s.terminal_font(), DEFAULT_MONO_FONT);
        s.mono_font_family = "Iosevka".into();
        assert_eq!(s.terminal_font(), "Iosevka");
        s.terminal.font_family = "Terminus".into();
        assert_eq!(s.terminal_font(), "Terminus");
    }

    #[test]
    fn zooming_stops_at_the_bounds_and_says_so() {
        let mut s = Settings::default();
        assert!(s.zoom(Zoom::Diff, 2.));
        assert_eq!(s.diff_font_size, 15.0);
        // The terminal does not move with the diffs.
        assert_eq!(s.terminal.font_size, 13.0);

        assert!(s.zoom(Zoom::Diff, 1_000.));
        assert_eq!(s.diff_font_size, MAX_FONT_SIZE);
        // At the end of the travel, nothing to redraw: that is what false says.
        assert!(!s.zoom(Zoom::Diff, 1.));

        assert!(s.reset_zoom(Zoom::Diff));
        assert_eq!(s.diff_font_size, Settings::default().diff_font_size);
        assert!(!s.reset_zoom(Zoom::Diff));
    }

    #[test]
    fn the_shell_setting_becomes_a_command_line() {
        let mut s = TerminalSettings::default();
        // Empty: it is the login shell, and nobody else has to decide which.
        assert_eq!(s.program(), None);
        s.shell = "   ".into();
        assert_eq!(s.program(), None);

        s.shell = "fish".into();
        assert_eq!(s.program(), Some(("fish".into(), vec![])));

        // A whole command line: opening a tmux session directly is a common use,
        // and a bare shell is not always what one wants.
        s.shell = "tmux new-session -A -s claudhub".into();
        assert_eq!(
            s.program(),
            Some((
                "tmux".into(),
                vec![
                    "new-session".into(),
                    "-A".into(),
                    "-s".into(),
                    "claudhub".into()
                ]
            ))
        );
    }

    #[test]
    fn the_old_agent_command_becomes_a_profile() {
        // The file of a user who has never seen the profiles. The path in it is
        // quoted, which the old splitting could not read: the pick-up takes the
        // chance to set it straight.
        let mut terminal: TerminalSettings =
            serde_json::from_str(r#"{"agent_command":"\"/opt/a b/claude\" --resume"}"#).unwrap();
        terminal.migrate_agents();
        assert_eq!(terminal.agents.len(), 1);
        assert_eq!(terminal.agents[0].command, "/opt/a b/claude");
        assert_eq!(terminal.agents[0].args, vec!["--resume"]);
        assert_eq!(terminal.agents[0].label(), "claude");
        // Emptied: the pick-up only happens once.
        assert!(terminal.agent_command.is_empty());

        // A fresh install goes down the same path.
        let mut fresh = TerminalSettings::default();
        fresh.migrate_agents();
        assert_eq!(fresh.agents, vec![AgentProfile::claude()]);

        // A file that already has profiles is not touched.
        let mut kept = TerminalSettings {
            agents: vec![AgentProfile {
                name: "aider".into(),
                command: "aider".into(),
                ..Default::default()
            }],
            agent_command: "claude".into(),
            ..Default::default()
        };
        kept.migrate_agents();
        assert_eq!(kept.agents.len(), 1);
        assert_eq!(kept.agents[0].name, "aider");
    }

    #[test]
    fn the_programs_to_look_for_are_all_the_profiles() {
        let terminal = TerminalSettings {
            agents: vec![
                AgentProfile {
                    name: "opus".into(),
                    command: "/usr/bin/claude".into(),
                    ..Default::default()
                },
                AgentProfile {
                    name: "sonnet".into(),
                    command: "claude".into(),
                    ..Default::default()
                },
                AgentProfile {
                    name: "aider".into(),
                    command: "aider".into(),
                    ..Default::default()
                },
            ],
            default_agent: "sonnet".into(),
            ..Default::default()
        };
        // Deduplicated on the program name: two profiles of the same agent do
        // not make it count twice in /proc.
        assert_eq!(terminal.agent_programs(), vec!["aider", "claude"]);
        assert_eq!(terminal.default_profile().unwrap().name, "sonnet");
    }

    #[test]
    fn an_environment_line_round_trips() {
        let mut profile = AgentProfile::claude();
        profile.set_env_line(r#"ANTHROPIC_MODEL=opus PROMPT="deux mots""#);
        assert_eq!(profile.env.get("ANTHROPIC_MODEL").unwrap(), "opus");
        assert_eq!(profile.env.get("PROMPT").unwrap(), "deux mots");
        let mut again = AgentProfile::claude();
        again.set_env_line(&profile.env_line());
        assert_eq!(again.env, profile.env);
    }

    #[test]
    fn monospace_names_are_recognised_by_convention() {
        for name in ["JetBrains Mono", "Fira Code", "Consolas", "Iosevka Term"] {
            assert!(is_monospace_name(name), "{name}");
        }
        for name in ["Inter", "Fira Sans", "Helvetica"] {
            assert!(!is_monospace_name(name), "{name}");
        }
    }

    #[test]
    fn font_choices_are_sorted_deduplicated_and_always_offer_the_default() {
        let installed = vec![
            "Fira Code".to_string(),
            "Inter".to_string(),
            ".SF NS Mono".to_string(),
            "fira code".to_string(),
        ];
        let mono = font_choices(&installed, true, DEFAULT_MONO_FONT);
        assert_eq!(mono, vec!["Fira Code", DEFAULT_MONO_FONT]);

        // Without the filter, the interface sees everything — except the private families.
        let all = font_choices(&installed, false, DEFAULT_UI_FONT);
        assert_eq!(all, vec!["Fira Code", "Inter", DEFAULT_UI_FONT]);
    }

    #[test]
    fn shells_ignore_comments_and_blank_lines() {
        let text = "# /etc/shells\n\n/bin/sh\n/bin/bash\n /usr/bin/fish \n";
        assert_eq!(
            parse_shells(text),
            vec!["/bin/sh", "/bin/bash", "/usr/bin/fish"]
        );
    }
}
