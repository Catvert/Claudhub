//! Keyboard shortcuts.
//!
//! A terminal needs almost every combination: Ctrl+C, Ctrl+D, Ctrl+L belong to
//! the program running in it, not to Claudhub. The application's shortcuts
//! therefore go through the platform key (`secondary-`, that is, Ctrl on Linux
//! and Windows, Cmd on macOS).
//!
//! Which is not enough: on Linux, `secondary` **is** Ctrl, and a binding on
//! `secondary-r` takes the shell's Ctrl+R — the reverse history search — without
//! saying anything. Hence two predicates and not one: what is written with a
//! single letter (`WINDOW_PREDICATE`) leaves the terminal alone, what needs
//! Shift or a function key (`PREDICATE`) holds everywhere. The terminals
//! themselves set that convention: Ctrl+Shift+C to copy, because Ctrl+C is
//! taken.
//!
//! **One letter is taken anyway**, and the exception says the rule: `Ctrl+T`
//! hides the terminals, and what it hides is the terminal one is typing in. A
//! binding that stopped at its edge would stop exactly where it is needed.
//!
//! **A single table describes each binding** (`table!`), and both `bind_keys`
//! and the help window come out of it. Two lists would have diverged on the
//! first addition, and help that lies about the keys is worse than no help.

use gpui::{actions, App, KeyBinding, KeyContext, SharedString, Window};

use crate::tr;
use crate::ui::app::ClaudhubApp;

actions!(
    claudhub,
    [
        Refresh,
        NewTerminal,
        CloseTerminal,
        ToggleTerminal,
        NextTerminal,
        PreviousTerminal,
        Commit,
        OpenSettings,
        ShowShortcuts,
        ToggleSidebar,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        Fetch,
        Pull,
        Push,
        CopyDiff,
        CopyDiffPatch,
        SelectWholeDiff,
        PreviousLine,
        NextLine,
        ExtendUp,
        ExtendDown,
        PreviousHunk,
        NextHunk,
        PreviousFile,
        NextFile,
        DiffStart,
        DiffEnd,
        DiffPageUp,
        DiffPageDown,
        ToggleDiffSplit,
        ToggleWholeFile,
        ToggleStage,
        ToggleReviewTree,
        AnnotateSelection,
        AskAgent,
        SendNotes,
        SaveFile,
        CloseEditor,
        Find,
        CloseFind,
        FindNext,
        FindPrevious,
        ExplorerUp,
        ExplorerDown,
        ExplorerLeft,
        ExplorerRight,
        ExplorerHome,
        ExplorerEnd,
        ExplorerOpen,
        DbUp,
        DbDown,
        DbLeft,
        DbRight,
        DbOpen,
        RunDbQuery,
        CopyDbResult,
        ExportDbCsv,
        SelectWholeResult
    ]
);

/// Go to the n-th screen.
///
/// An action *with a payload* rather than four actions, as for the worktrees:
/// `Alt+1` to `Alt+4` do the same thing up to an index.
#[derive(Clone, PartialEq, Debug, Default, gpui::Action)]
#[action(namespace = claudhub, no_json)]
pub struct GoToWorkspace {
    pub index: usize,
}

/// Go to the n-th worktree in the sidebar.
///
/// An action *with a payload* rather than nine actions: `Ctrl+1` to `Ctrl+9` do
/// the same thing up to an index, and nine identical handlers would say nothing
/// more.
#[derive(Clone, PartialEq, Debug, Default, gpui::Action)]
#[action(namespace = claudhub, no_json)]
pub struct SelectWorktree {
    pub index: usize,
}

/// The bindings' predicate. gpui-component's layers (dialog, menu, popover) are
/// excluded: a shortcut firing behind a dialog acts on state the user is not
/// looking at.
///
/// Not to be confused with `context()`: this is an *expression*, evaluated
/// against the focused node's context stack, and it only makes sense inside
/// `KeyBinding::new`. Passing it to `key_context` makes the parser loop.
const PREDICATE: &str = "Claudhub && !Dialog && !PopupMenu && !Popover";

/// The commit confirmation's predicate.
///
/// `Ctrl+Enter` is also the key that runs a query in every SQL console one
/// already has under one's fingers. The two cannot coexist on the same key
/// without one taking the other, and it is the console that wins when one is
/// typing in it: it is deeper in the context stack, but the exclusion is written
/// out rather than inferred — resolution by depth is exactly the kind of thing
/// nobody rereads.
const COMMIT_PREDICATE: &str = "Claudhub && !Dialog && !PopupMenu && !Popover && !ClaudhubQuery";

/// The predicate of what is written with the platform key and **a single letter**.
///
/// On Linux, `secondary-s` *is* Ctrl+S, that is, XOFF, and `secondary-r` is the
/// shell's reverse search. A binding that held in the terminal too would take
/// them silently — and the agent running in it is precisely what one came to
/// drive.
const WINDOW_PREDICATE: &str = "Claudhub && !Dialog && !PopupMenu && !Popover && !ClaudhubTerminal";

/// The predicate of copying from the diff.
///
/// `Ctrl+C` belongs first to whoever has the focus: the commit message field has
/// its own copy, and the terminal passes the key to the running program. Without
/// those two exclusions, copying a line typed into the commit message would give
/// the diff instead.
const COPY_PREDICATE: &str = "Claudhub && !Dialog && !PopupMenu && !Popover && !Input \
     && !ClaudhubTerminal && !ClaudhubQuery";

/// The predicate of copying from the result grid.
///
/// The console takes the diff's place: `Ctrl+C` there copies a cell or the
/// result, never the reviewed file — hence the reciprocal exclusion in
/// `COPY_PREDICATE`. The query editor keeps its own, like the commit message
/// field.
const QUERY_COPY_PREDICATE: &str = "ClaudhubQuery && !Input && !PopupMenu && !Popover";

/// The keyboard navigation predicate.
///
/// The bare arrows are the only Claudhub keys that do not go through the
/// platform key, and that is what makes them delicate: they belong to whoever
/// has the focus. An input field moves its cursor, a terminal passes them to the
/// program, a menu changes entry — those three are therefore excluded, as for
/// copying.
///
/// The explorer is excluded in turn: its arrows belong to it — one browses a
/// tree there, not a diff — and two sets of bindings on the same key would not
/// be settled.
const NAVIGATION_PREDICATE: &str = "Claudhub && !Dialog && !PopupMenu && !Popover && !Input \
     && !ClaudhubTerminal && !ClaudhubExplorer && !ClaudhubDb";

/// The vim navigation predicate.
///
/// `ClaudhubVim` is **on the same node** as `Claudhub` — the root view — and
/// that is not a style detail: `depth_of` evaluates each identifier against one
/// single level of the context stack, so two identifiers declared at two
/// different depths never meet in an `&&`.
const VIM_PREDICATE: &str = "Claudhub && ClaudhubVim && !Dialog && !PopupMenu && !Popover \
     && !Input && !ClaudhubTerminal && !ClaudhubExplorer && !ClaudhubDb";

/// The context the root view declares. Identifiers, not a predicate: it is the
/// name `PREDICATE` refers to.
///
/// `ClaudhubVim` is added to it when vim mode is on, and that is enough to turn
/// it on or off: the context is recomputed on every render, whereas the bindings
/// are installed once and for all at startup.
pub fn context(vim: bool) -> KeyContext {
    let mut context = KeyContext::default();
    context.add("Claudhub");
    if vim {
        context.add("ClaudhubVim");
    }
    context
}

// Actions handled by the focused terminal, and not by the window.
// The names carry their object (`CopySelection` rather than `Copy`): an action
// named `Copy` would collide with the trait of the same name, which every Rust
// module has in scope.
actions!(
    claudhub_terminal,
    [CopySelection, PasteClipboard, SelectAllText]
);

/// The context a terminal view declares. The three shortcuts below only exist
/// there: `Ctrl+Shift+C` elsewhere in the interface would have nothing to copy,
/// and a plain `Ctrl+C` belongs to the running program.
const TERMINAL_PREDICATE: &str = "ClaudhubTerminal";

pub fn terminal_context() -> KeyContext {
    let mut context = KeyContext::default();
    context.add("ClaudhubTerminal");
    context
}

/// The context a search bar declares.
///
/// `Esc` closes it, and has nothing to close elsewhere: binding it globally
/// would turn a universal cancel key into one panel's gesture.
const FIND_PREDICATE: &str = "ClaudhubFind";

pub fn find_context() -> KeyContext {
    let mut context = KeyContext::default();
    context.add("ClaudhubFind");
    context
}

/// The context the explorer's tree declares.
///
/// The arrows browse a tree there: up and down from one row to the next, right
/// to unfold, left to collapse or to go up to the parent folder. They are
/// PhpStorm's, and every explorer's.
const EXPLORER_PREDICATE: &str = "ClaudhubExplorer";

/// The same in vim mode. `ClaudhubVim` has to be declared **by the tree itself**
/// and not by the root: see `VIM_PREDICATE`.
const VIM_EXPLORER_PREDICATE: &str = "ClaudhubExplorer && ClaudhubVim";

/// The context the databases tree declares.
///
/// The same arrows as the project explorer, on another tree: whichever has the
/// focus takes them. Without this context they would belong to diff review, and
/// browsing a schema would scroll the code beside it.
const DB_PREDICATE: &str = "ClaudhubDb";

/// The same in vim mode. `ClaudhubVim` has to be declared **by the tree
/// itself**: see `VIM_PREDICATE`.
const VIM_DB_PREDICATE: &str = "ClaudhubDb && ClaudhubVim";

pub fn db_context(vim: bool) -> KeyContext {
    let mut context = KeyContext::default();
    context.add("ClaudhubDb");
    if vim {
        context.add("ClaudhubVim");
    }
    context
}

/// The context the SQL console declares.
///
/// `Ctrl+Enter` runs the query there rather than confirming a commit; it is
/// every SQL console's convention.
const QUERY_PREDICATE: &str = "ClaudhubQuery";

pub fn query_context() -> KeyContext {
    let mut context = KeyContext::default();
    context.add("ClaudhubQuery");
    context
}

pub fn explorer_context(vim: bool) -> KeyContext {
    let mut context = KeyContext::default();
    context.add("ClaudhubExplorer");
    if vim {
        context.add("ClaudhubVim");
    }
    context
}

/// The help's families, in the order it shows them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    Window,
    Worktrees,
    Repository,
    Review,
    Explorer,
    Database,
    Search,
    Terminal,
}

impl Group {
    pub const ORDER: [Group; 8] = [
        Group::Window,
        Group::Worktrees,
        Group::Repository,
        Group::Review,
        Group::Explorer,
        Group::Database,
        Group::Search,
        Group::Terminal,
    ];

    /// The family's name in a binding's id, and therefore in `settings.json`.
    ///
    /// Short and stable: it is written into the user's file, and renaming it
    /// would silently drop the shortcuts they had customised.
    pub fn slug(self) -> &'static str {
        match self {
            Group::Window => "window",
            Group::Worktrees => "worktree",
            Group::Repository => "repo",
            Group::Review => "review",
            Group::Explorer => "explorer",
            Group::Database => "db",
            Group::Search => "search",
            Group::Terminal => "terminal",
        }
    }

    /// The i18n key of the title. The key and not the text: a test checks that
    /// all of this module's exist in both catalogues, and it can only do that on
    /// keys.
    pub fn key(self) -> &'static str {
        match self {
            Group::Window => "shortcut-group-window",
            Group::Worktrees => "shortcut-group-worktrees",
            Group::Repository => "shortcut-group-repository",
            Group::Review => "shortcut-group-review",
            Group::Explorer => "shortcut-group-explorer",
            Group::Database => "shortcut-group-database",
            Group::Search => "shortcut-group-search",
            Group::Terminal => "shortcut-group-terminal",
        }
    }
}

/// A binding, as the help shows it.
///
/// The same record serves `bind_keys`: it is the only way to be sure the help
/// says what the keyboard does.
pub struct Entry {
    pub keys: &'static str,
    pub group: Group,
    /// The i18n key of the description.
    pub label: &'static str,
    /// Two bindings may carry the same keys — `Enter` opens a file in the
    /// explorer and goes to the next hit in a search — provided their
    /// predicates never meet. It is also what says whether two keys the user
    /// has customised collide.
    pub predicate: &'static str,
    /// Only under vim mode. The settings page says so rather than offering a
    /// key that does nothing while the mode is off.
    pub vim: bool,
}

/// The overrides, as `settings.json` carries them: binding id → keystrokes.
///
/// An empty value **disables** the binding rather than removing the entry: the
/// line stays in the file, and one sees what one has switched off.
pub type Overrides = std::collections::BTreeMap<String, String>;

impl Entry {
    /// What names this binding in the settings, for good.
    ///
    /// The family and the **default** keys: the default never moves, so an id
    /// survives the customisation it carries. The action would not do — half
    /// of them have two bindings, `F5` and `Ctrl+R` for one refresh — and the
    /// keys alone would not either: `Enter` exists in three families.
    pub fn id(&self) -> String {
        format!("{}:{}", self.group.slug(), self.keys)
    }

    /// The keys in force: the user's, else ours. Empty when switched off.
    pub fn effective<'a>(&'a self, overrides: &'a Overrides) -> &'a str {
        overrides
            .get(&self.id())
            .map(String::as_str)
            .unwrap_or(self.keys)
    }
}

/// The keys that have a name rather than a character.
///
/// Written out because gpui does not offer the list: its own is a *negative*
/// one, buried in a private function. Without it a typo would pass — `f5` and
/// `f6` are readable, and so is `nonsense`, which parses perfectly and then
/// matches nothing for the rest of the session.
const NAMED_KEYS: &[&str] = &[
    "escape",
    "enter",
    "tab",
    "space",
    "backspace",
    "delete",
    "insert",
    "home",
    "end",
    "pageup",
    "pagedown",
    "up",
    "down",
    "left",
    "right",
    "back",
    "forward",
    "f1",
    "f2",
    "f3",
    "f4",
    "f5",
    "f6",
    "f7",
    "f8",
    "f9",
    "f10",
    "f11",
    "f12",
    "f13",
    "f14",
    "f15",
    "f16",
    "f17",
    "f18",
    "f19",
    "f20",
];

/// Can gpui read these keystrokes, and will they ever fire?
///
/// Two things, and the second is the one that matters. `KeyBinding::new`
/// **panics** on what it cannot parse, and this text comes from a form: it is
/// checked before, one keystroke at a time, exactly as it will be split. But
/// gpui parses almost anything — `ctrl-nonsense` is a perfectly good keystroke
/// on a key no keyboard has — and such a binding is worse than a refused one:
/// it is installed, it never fires, and nothing says why. The key must
/// therefore be a single character or one of the names above.
///
/// An empty text is not invalid — it is a binding switched off.
pub fn valid_keys(keys: &str) -> bool {
    keys.split_whitespace().all(|stroke| {
        gpui::Keystroke::parse(stroke).is_ok_and(|stroke| {
            stroke.key.chars().count() == 1 || NAMED_KEYS.contains(&stroke.key.as_str())
        })
    })
}

/// Declares a family of bindings: the keys on one side, the help on the other,
/// written once only.
macro_rules! table {
    ($entries:ident, $bind:ident, $vim:literal, [
        $($group:ident $keys:literal => $action:expr, $predicate:expr, $label:literal;)*
    ]) => {
        static $entries: &[Entry] = &[$(
            Entry {
                keys: $keys,
                group: Group::$group,
                label: $label,
                predicate: $predicate,
                vim: $vim,
            },
        )*];

        /// The bindings to install, the user's customisations applied.
        ///
        /// A binding whose keys are empty is **not installed**: that is what
        /// switching one off means. One that does not parse is skipped and
        /// logged rather than crashing the window — `KeyBinding::new` panics,
        /// and this text comes from a form; `valid_keys` is what the form
        /// checks with, this is the belt to its braces.
        fn $bind(overrides: &Overrides) -> Vec<KeyBinding> {
            let mut out = Vec::new();
            let mut entries = $entries.iter();
            $({
                let keys = entries
                    .next()
                    .map(|entry| entry.effective(overrides))
                    .unwrap_or($keys);
                if keys.trim().is_empty() {
                } else if valid_keys(keys) {
                    out.push(KeyBinding::new(keys, $action, Some($predicate)));
                } else {
                    log::warn!("unreadable shortcut {keys:?}, keeping none");
                }
            })*
            out
        }
    };
}

table!(STANDARD, standard_bindings, false, [
    // ── The window ──────────────────────────────────────────────────────────
    Window "f1" => ShowShortcuts, PREDICATE, "shortcut-help";
    Window "f5" => Refresh, PREDICATE, "shortcut-refresh";
    Window "secondary-r" => Refresh, WINDOW_PREDICATE, "shortcut-refresh";
    // Every editor's convention, including on Linux.
    Window "secondary-," => OpenSettings, PREDICATE, "shortcut-settings";
    Window "secondary-b" => ToggleSidebar, WINDOW_PREDICATE, "shortcut-sidebar";
    // Zoom aims at the area that has the focus: the terminal when it has it,
    // the diffs otherwise. `secondary-=` as much as `secondary-+` because the
    // plus sign needs Shift on an azerty keyboard as on a qwerty one.
    Window "secondary-=" => ZoomIn, PREDICATE, "shortcut-zoom-in";
    Window "secondary-+" => ZoomIn, PREDICATE, "shortcut-zoom-in";
    Window "secondary--" => ZoomOut, PREDICATE, "shortcut-zoom-out";
    Window "secondary-0" => ZoomReset, PREDICATE, "shortcut-zoom-reset";

    // ── The screens ─────────────────────────────────────────────────────────
    // Four bindings and a single help line, like the worktrees.
    //
    // **Alt and not `secondary-shift`.** gpui **removes** Shift from the
    // modifiers when the key is a caseless character: `secondary-shift-1`
    // arrives as `ctrl-&` or `ctrl-#` depending on the keyboard layout, and the
    // binding never fires — silently. Alt, for its part, is kept, and the key
    // stays the digit. It is also the convention of whoever switches tabs.
    //
    // Valid right into the terminal: what we take from it is readline's numeric
    // argument prefix (`M-1`), and not a control character like `Ctrl+R` — see
    // `WINDOW_PREDICATE`.
    Window "alt-1" => GoToWorkspace { index: 0 }, PREDICATE, "shortcut-workspace";
    Window "alt-2" => GoToWorkspace { index: 1 }, PREDICATE, "shortcut-workspace";
    Window "alt-3" => GoToWorkspace { index: 2 }, PREDICATE, "shortcut-workspace";
    Window "alt-4" => GoToWorkspace { index: 3 }, PREDICATE, "shortcut-workspace";
    Window "alt-5" => GoToWorkspace { index: 4 }, PREDICATE, "shortcut-workspace";

    // ── The worktrees ───────────────────────────────────────────────────────
    // Nine bindings and a single help line: `merge` recognises the run of digits
    // and shows it as a range.
    Worktrees "secondary-1" => SelectWorktree { index: 0 }, PREDICATE, "shortcut-worktree";
    Worktrees "secondary-2" => SelectWorktree { index: 1 }, PREDICATE, "shortcut-worktree";
    Worktrees "secondary-3" => SelectWorktree { index: 2 }, PREDICATE, "shortcut-worktree";
    Worktrees "secondary-4" => SelectWorktree { index: 3 }, PREDICATE, "shortcut-worktree";
    Worktrees "secondary-5" => SelectWorktree { index: 4 }, PREDICATE, "shortcut-worktree";
    Worktrees "secondary-6" => SelectWorktree { index: 5 }, PREDICATE, "shortcut-worktree";
    Worktrees "secondary-7" => SelectWorktree { index: 6 }, PREDICATE, "shortcut-worktree";
    Worktrees "secondary-8" => SelectWorktree { index: 7 }, PREDICATE, "shortcut-worktree";
    Worktrees "secondary-9" => SelectWorktree { index: 8 }, PREDICATE, "shortcut-worktree";

    // ── The repository ──────────────────────────────────────────────────────
    // With Shift, so valid right into the terminal: those three go out over the
    // network and do not depend on what is being looked at.
    Repository "secondary-shift-r" => Fetch, PREDICATE, "shortcut-fetch";
    Repository "secondary-shift-u" => Pull, PREDICATE, "shortcut-pull";
    Repository "secondary-shift-p" => Push, PREDICATE, "shortcut-push";
    Repository "secondary-enter" => Commit, COMMIT_PREDICATE, "shortcut-commit";

    // ── Review ──────────────────────────────────────────────────────────────
    // The bare arrows go from one change to the next — that is the review
    // gesture, the context lines between two hunks having nothing to show — and
    // overflow onto the neighbouring file once the last hunk is passed. The
    // platform key steps by one line, Shift extends the selection.
    Review "up" => PreviousHunk, NAVIGATION_PREDICATE, "shortcut-previous-hunk";
    Review "down" => NextHunk, NAVIGATION_PREDICATE, "shortcut-next-hunk";
    Review "secondary-up" => PreviousLine, NAVIGATION_PREDICATE, "shortcut-previous-line";
    Review "secondary-down" => NextLine, NAVIGATION_PREDICATE, "shortcut-next-line";
    Review "shift-up" => ExtendUp, NAVIGATION_PREDICATE, "shortcut-extend-up";
    Review "shift-down" => ExtendDown, NAVIGATION_PREDICATE, "shortcut-extend-down";
    Review "left" => PreviousFile, NAVIGATION_PREDICATE, "shortcut-previous-file";
    Review "right" => NextFile, NAVIGATION_PREDICATE, "shortcut-next-file";
    Review "pageup" => DiffPageUp, NAVIGATION_PREDICATE, "shortcut-page-up";
    Review "pagedown" => DiffPageDown, NAVIGATION_PREDICATE, "shortcut-page-down";
    Review "home" => DiffStart, NAVIGATION_PREDICATE, "shortcut-diff-start";
    Review "end" => DiffEnd, NAVIGATION_PREDICATE, "shortcut-diff-end";
    // Copy the reviewed code, and its variant that keeps the patch markers.
    Review "secondary-c" => CopyDiff, COPY_PREDICATE, "shortcut-copy";
    Review "secondary-shift-c" => CopyDiffPatch, COPY_PREDICATE, "shortcut-copy-patch";
    Review "secondary-a" => SelectWholeDiff, COPY_PREDICATE, "shortcut-select-all";
    // Annotating and asking share the copy predicate: they start from a
    // selection in the diff, and have nothing to do when an input field or a
    // terminal has the focus.
    Review "secondary-shift-n" => AnnotateSelection, COPY_PREDICATE, "shortcut-annotate";
    Review "secondary-shift-k" => AskAgent, COPY_PREDICATE, "shortcut-ask";
    Review "secondary-shift-e" => SendNotes, PREDICATE, "shortcut-send-notes";
    Review "secondary-shift-s" => ToggleDiffSplit, PREDICATE, "shortcut-split";
    Review "secondary-shift-f" => ToggleWholeFile, PREDICATE, "shortcut-whole-file";
    Review "secondary-shift-i" => ToggleStage, PREDICATE, "shortcut-stage";
    Review "secondary-shift-l" => ToggleReviewTree, PREDICATE, "shortcut-review-tree";
    // Save and close target the built-in editor; in the terminal, Ctrl+S is XOFF
    // and Ctrl+W deletes a word.
    Review "secondary-s" => SaveFile, WINDOW_PREDICATE, "shortcut-save";
    Review "secondary-w" => CloseEditor, WINDOW_PREDICATE, "shortcut-close-editor";

    // ── The explorer   ───────────────────────────────────────────────────────
    Explorer "up" => ExplorerUp, EXPLORER_PREDICATE, "shortcut-explorer-up";
    Explorer "down" => ExplorerDown, EXPLORER_PREDICATE, "shortcut-explorer-down";
    Explorer "left" => ExplorerLeft, EXPLORER_PREDICATE, "shortcut-explorer-collapse";
    Explorer "right" => ExplorerRight, EXPLORER_PREDICATE, "shortcut-explorer-expand";
    Explorer "home" => ExplorerHome, EXPLORER_PREDICATE, "shortcut-explorer-first";
    Explorer "end" => ExplorerEnd, EXPLORER_PREDICATE, "shortcut-explorer-last";
    Explorer "enter" => ExplorerOpen, EXPLORER_PREDICATE, "shortcut-explorer-open";

    // ── The databases ───────────────────────────────────────────────────────
    // The same set as the explorer, on another tree: whichever has the focus
    // takes them.
    Database "up" => DbUp, DB_PREDICATE, "shortcut-db-up";
    Database "down" => DbDown, DB_PREDICATE, "shortcut-db-down";
    Database "left" => DbLeft, DB_PREDICATE, "shortcut-db-collapse";
    Database "right" => DbRight, DB_PREDICATE, "shortcut-db-expand";
    Database "enter" => DbOpen, DB_PREDICATE, "shortcut-db-open";
    Database "secondary-enter" => RunDbQuery, QUERY_PREDICATE, "shortcut-db-run";
    Database "secondary-c" => CopyDbResult, QUERY_COPY_PREDICATE, "shortcut-db-copy";
    Database "secondary-a" => SelectWholeResult, QUERY_COPY_PREDICATE, "shortcut-db-select-all";
    Database "secondary-shift-e" => ExportDbCsv, QUERY_PREDICATE, "shortcut-db-export";

    // ── Search ──────────────────────────────────────────────────────────────
    // `Ctrl+F` searches in the panel where the last click happened. It is
    // excluded from the terminal and from input fields, which each have their own.
    Search "secondary-f" => Find, COPY_PREDICATE, "shortcut-find";
    Search "secondary-g" => FindNext, WINDOW_PREDICATE, "shortcut-find-next";
    Search "enter" => FindNext, FIND_PREDICATE, "shortcut-find-next";
    Search "secondary-shift-g" => FindPrevious, PREDICATE, "shortcut-find-previous";
    Search "shift-enter" => FindPrevious, FIND_PREDICATE, "shortcut-find-previous";
    Search "escape" => CloseFind, FIND_PREDICATE, "shortcut-close-find";

    // ── The terminals ───────────────────────────────────────────────────────
    Terminal "secondary-shift-t" => NewTerminal, PREDICATE, "shortcut-new-terminal";
    Terminal "secondary-shift-w" => CloseTerminal, PREDICATE, "shortcut-close-terminal";
    // **The one letter taken from the terminal**, and the exception is the
    // point: what this gesture does is hide the terminal one is typing in, so a
    // binding that stops at its edge stops exactly where it is needed. Having
    // to click elsewhere first to be allowed to close it is not a shortcut.
    //
    // It used to leave the terminal alone, with `secondary-\`` as the way back
    // out. That backtick does not exist on an AZERTY keyboard — it is a dead key
    // behind AltGr — so on half the keyboards there was no way at all. What is
    // taken from the running program is readline's `transpose-chars`.
    Terminal "secondary-t" => ToggleTerminal, PREDICATE, "shortcut-toggle-terminal";
    Terminal "secondary-tab" => NextTerminal, PREDICATE, "shortcut-next-terminal";
    Terminal "secondary-shift-tab" => PreviousTerminal, PREDICATE, "shortcut-previous-terminal";
    // The terminals' conventions: the platform key *with* Shift, because a bare
    // `Ctrl+C` and `Ctrl+V` belong to the program.
    Terminal "secondary-shift-c" => CopySelection, TERMINAL_PREDICATE, "shortcut-terminal-copy";
    Terminal "secondary-shift-v" => PasteClipboard, TERMINAL_PREDICATE, "shortcut-terminal-paste";
    Terminal "secondary-shift-a" => SelectAllText, TERMINAL_PREDICATE, "shortcut-terminal-select-all";
]);

table!(VIM, vim_bindings, true, [
    // No modes and no operators: Claudhub is not an editor, and its built-in
    // editor belongs to gpui-component. What is taken over is the left hand on
    // the home row to browse a diff — what a reviewer does a thousand times per
    // review.
    // `j` and `k` go from one **hunk** to the next, like the bare arrows and for
    // the same reason: reading a review is going from one change to the next,
    // and the context lines in between have nothing to show. The platform key
    // steps by one line, exactly as `secondary-up`/`down` does — bare for the
    // gesture one makes a thousand times, modified for the one one makes when
    // something looks wrong.
    Review "j" => NextHunk, VIM_PREDICATE, "shortcut-next-hunk";
    Review "k" => PreviousHunk, VIM_PREDICATE, "shortcut-previous-hunk";
    Review "secondary-j" => NextLine, VIM_PREDICATE, "shortcut-next-line";
    Review "secondary-k" => PreviousLine, VIM_PREDICATE, "shortcut-previous-line";
    // vim-gitgutter's and fugitive's convention for the same gesture. Kept
    // beside `j`/`k`: it is what the fingers of whoever reviews in vim do, and
    // the help sheet puts the two ways on one line.
    Review "] c" => NextHunk, VIM_PREDICATE, "shortcut-next-hunk";
    Review "[ c" => PreviousHunk, VIM_PREDICATE, "shortcut-previous-hunk";
    Review "l" => NextFile, VIM_PREDICATE, "shortcut-next-file";
    Review "h" => PreviousFile, VIM_PREDICATE, "shortcut-previous-file";
    Review "g g" => DiffStart, VIM_PREDICATE, "shortcut-diff-start";
    Review "shift-g" => DiffEnd, VIM_PREDICATE, "shortcut-diff-end";
    Review "secondary-d" => DiffPageDown, VIM_PREDICATE, "shortcut-page-down";
    Review "secondary-u" => DiffPageUp, VIM_PREDICATE, "shortcut-page-up";
    Review "y" => CopyDiff, VIM_PREDICATE, "shortcut-copy";

    Explorer "j" => ExplorerDown, VIM_EXPLORER_PREDICATE, "shortcut-explorer-down";
    Explorer "k" => ExplorerUp, VIM_EXPLORER_PREDICATE, "shortcut-explorer-up";
    Explorer "l" => ExplorerRight, VIM_EXPLORER_PREDICATE, "shortcut-explorer-expand";
    Explorer "h" => ExplorerLeft, VIM_EXPLORER_PREDICATE, "shortcut-explorer-collapse";
    Explorer "g g" => ExplorerHome, VIM_EXPLORER_PREDICATE, "shortcut-explorer-first";
    Explorer "shift-g" => ExplorerEnd, VIM_EXPLORER_PREDICATE, "shortcut-explorer-last";

    Database "j" => DbDown, VIM_DB_PREDICATE, "shortcut-db-down";
    Database "k" => DbUp, VIM_DB_PREDICATE, "shortcut-db-up";
    Database "l" => DbRight, VIM_DB_PREDICATE, "shortcut-db-expand";
    Database "h" => DbLeft, VIM_DB_PREDICATE, "shortcut-db-collapse";

    Search "/" => Find, VIM_PREDICATE, "shortcut-find";
    Search "n" => FindNext, VIM_PREDICATE, "shortcut-find-next";
    Search "shift-n" => FindPrevious, VIM_PREDICATE, "shortcut-find-previous";
]);

pub fn init(cx: &mut App) {
    // What gpui-component bound before us, kept aside. Rebinding means clearing
    // the keymap — there is one for the whole application — and the library's
    // own bindings would go with it: the built-in editor would lose its
    // arrows, dialogs their Escape, and nothing public reinstalls them.
    // Hence the snapshot, taken **before** ours are added and while the
    // library's are all there. Whence the order in `ui::run`:
    // `gpui_component::init` first, since it is what we are keeping, then the
    // settings global, which `install` reads the customisations from, and only
    // then this.
    let base: Vec<KeyBinding> = cx.key_bindings().borrow().bindings().cloned().collect();
    cx.set_global(BaseKeymap(base));
    install(cx);
}

/// The bindings that were there before ours.
struct BaseKeymap(Vec<KeyBinding>);

impl gpui::Global for BaseKeymap {}

/// Installs ours, the user's customisations applied.
fn install(cx: &mut App) {
    let overrides = crate::ui::settings::Settings::global(cx).shortcuts.clone();
    // The vim bindings are installed **always**, and it is the `ClaudhubVim`
    // context that turns them on: the setting changes along the way, whereas
    // the keymap is written here.
    cx.bind_keys(standard_bindings(&overrides));
    cx.bind_keys(vim_bindings(&overrides));
}

/// A keystroke the user has just pressed, written the way the tables write it.
///
/// `None` for a modifier held on its own: gpui reports it as the key itself,
/// and a capture must wait for the one that follows rather than record `Ctrl`.
///
/// The platform key comes out as `secondary`, which is what the tables say and
/// what makes a customised binding read the same on the three platforms — gpui
/// parses it back to Ctrl here and Cmd on macOS.
pub fn stroke_syntax(stroke: &gpui::Keystroke) -> Option<String> {
    let key = stroke.key.as_str();
    if matches!(
        key,
        "shift" | "control" | "alt" | "platform" | "function" | "ctrl" | "cmd"
    ) {
        return None;
    }
    let modifiers = &stroke.modifiers;
    let mut parts: Vec<&str> = Vec::new();
    if cfg!(target_os = "macos") {
        if modifiers.platform {
            parts.push("secondary");
        }
        if modifiers.control {
            parts.push("ctrl");
        }
    } else {
        if modifiers.control {
            parts.push("secondary");
        }
        if modifiers.platform {
            parts.push("super");
        }
    }
    if modifiers.alt {
        parts.push("alt");
    }
    if modifiers.shift {
        parts.push("shift");
    }
    parts.push(key);
    Some(parts.join("-"))
}

/// Every binding, in the order the tables declare them: the settings page's
/// list, and the only one there is.
pub fn all() -> impl Iterator<Item = &'static Entry> {
    STANDARD.iter().chain(VIM.iter())
}

/// Puts the keymap back together after a shortcut has been customised.
///
/// A binding cannot be *replaced*: gpui's keymap only takes additions, and the
/// last one wins — the old key would go on firing beside the new one. The whole
/// map is therefore rebuilt, the library's snapshot first so that ours keep the
/// last word.
pub fn rebind(cx: &mut App) {
    let base = cx.global::<BaseKeymap>().0.clone();
    cx.clear_key_bindings();
    cx.bind_keys(base);
    install(cx);
}

/// A help family, ready to display.
pub struct Section {
    pub title: SharedString,
    pub rows: Vec<Row>,
}

pub struct Row {
    pub keys: String,
    pub label: SharedString,
}

/// The shortcuts, grouped, as the help window shows them.
///
/// The vim bindings only appear when the mode is on: showing them greyed out
/// would make a list twice as long, half of which does not work.
pub fn sheet(vim: bool, overrides: &Overrides) -> Vec<Section> {
    let labels = Labels::current();
    let mut sections = Vec::new();
    for group in Group::ORDER {
        let mut rows: Vec<Row> = Vec::new();
        // Two bindings for one gesture — F5 and Ctrl+R, Ctrl+1 to Ctrl+9, the
        // arrow and its vim equivalent — fit on one line: it is the gesture one
        // looks for in this list, not the key.
        let mut push = |entry: &Entry, keys: String| {
            let label = tr!(entry.label);
            match rows.iter_mut().find(|row| row.label == label) {
                Some(row) => row.keys = merge(&row.keys, &keys),
                None => rows.push(Row { keys, label }),
            }
        };
        // The keys **in force**, the user's customisations applied: help that
        // lies about the keys is worse than no help, and a binding switched off
        // has nothing to show.
        for entry in STANDARD.iter().filter(|e| e.group == group) {
            let keys = entry.effective(overrides);
            if !keys.trim().is_empty() {
                push(entry, pretty(keys, &labels));
            }
        }
        if vim {
            for entry in VIM.iter().filter(|e| e.group == group) {
                let keys = entry.effective(overrides);
                if !keys.trim().is_empty() {
                    push(entry, vim_pretty(keys));
                }
            }
        }
        if !rows.is_empty() {
            sections.push(Section {
                title: tr!(group.key()),
                rows,
            });
        }
    }
    sections
}

/// The words a key's rendering borrows from the language.
///
/// Passed as an argument rather than read with `tr!` deep inside the function:
/// that is what makes `pretty` free and testable, and the catalogue is not
/// loaded in a unit test.
pub struct Labels {
    pub shift: SharedString,
    pub escape: SharedString,
    pub enter: SharedString,
    pub home: SharedString,
    pub end: SharedString,
}

impl Labels {
    pub fn current() -> Self {
        Self {
            shift: tr!("key-shift"),
            escape: tr!("key-escape"),
            enter: tr!("key-enter"),
            home: tr!("key-home"),
            end: tr!("key-end"),
        }
    }
}

/// The platform key's name, as its keyboard spells it.
const SECONDARY: &str = if cfg!(target_os = "macos") {
    "⌘"
} else {
    "Ctrl"
};

/// A gpui binding rendered readable: `secondary-shift-e` → `Ctrl+Shift+E`.
pub fn pretty(keys: &str, labels: &Labels) -> String {
    keys.split(' ')
        .map(|stroke| {
            let mut parts: Vec<String> = Vec::new();
            let mut rest = stroke;
            // The key's name may be a dash (`secondary--`): it is the *last*
            // segment, never a modifier.
            while let Some((head, tail)) = rest.split_once('-') {
                match head {
                    "secondary" | "cmd" | "ctrl" => parts.push(SECONDARY.to_string()),
                    "shift" => parts.push(labels.shift.to_string()),
                    "alt" => parts.push("Alt".to_string()),
                    _ => break,
                }
                rest = tail;
            }
            parts.push(key_name(rest, labels));
            parts.join("+")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn key_name(key: &str, labels: &Labels) -> String {
    match key {
        "escape" => labels.escape.to_string(),
        "enter" => labels.enter.to_string(),
        "home" => labels.home.to_string(),
        "end" => labels.end.to_string(),
        "tab" => "Tab".to_string(),
        "space" => "␣".to_string(),
        "up" => "↑".to_string(),
        "down" => "↓".to_string(),
        "left" => "←".to_string(),
        "right" => "→".to_string(),
        "pageup" => "Page ↑".to_string(),
        "pagedown" => "Page ↓".to_string(),
        // Function keys are written in upper case, letters too, and the rest —
        // `,` `-` `` ` `` — as it is.
        other => other.to_uppercase(),
    }
}

/// A vim binding rendered **the way vim writes it**: `g g` → `gg`, `shift-g` →
/// `G`, `] c` → `]c`.
///
/// Translating those keys like the others would give "Shift+G" where everything
/// the user knows says "G": the notation is part of what they already know, and
/// replacing it would be teaching them something else.
pub fn vim_pretty(keys: &str) -> String {
    keys.split(' ')
        .map(|stroke| match stroke.split_once('-') {
            Some(("shift", key)) => key.to_uppercase(),
            Some(("secondary", key)) => format!("{SECONDARY}+{}", key.to_uppercase()),
            _ => stroke.to_string(),
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Joins two ways of making the same gesture onto a single line.
///
/// A run of numbered keys — `Ctrl+1` … `Ctrl+9` — is written as a range; two
/// unrelated keys are written as one or the other.
fn merge(current: &str, next: &str) -> String {
    if let Some(start) = current.split(['…', '/']).next().map(str::trim) {
        if consecutive(start, next) || current.contains('…') {
            let first = current.split('…').next().unwrap_or(current).trim();
            return format!("{first} … {next}");
        }
    }
    format!("{current} / {next}")
}

/// Two keys differing only by a digit that follows on.
///
/// The split is done on the **character** and not on the byte: a key is commonly
/// written `Ctrl+↓`, and cutting one step before the end of an arrow is a panic.
fn consecutive(first: &str, next: &str) -> bool {
    fn trailing_digit(text: &str) -> Option<(&str, u32)> {
        let last = text.chars().next_back()?;
        let digit = last.to_digit(10)?;
        Some((&text[..text.len() - last.len_utf8()], digit))
    }
    match (trailing_digit(first), trailing_digit(next)) {
        (Some((head, a)), Some((other, b))) => head == other && b == a + 1,
        _ => false,
    }
}

pub fn refresh(
    this: &mut ClaudhubApp,
    _: &Refresh,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.refresh_active(cx);
}

pub fn new_terminal(
    this: &mut ClaudhubApp,
    _: &NewTerminal,
    window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    let Some(worktree) = this.active_path() else {
        return;
    };
    this.show_terminal_panel(window, cx);
    this.open_terminal(
        &worktree,
        crate::ui::terminal_view::Launch::shell(),
        window,
        cx,
    );
}

pub fn close_terminal(
    this: &mut ClaudhubApp,
    _: &CloseTerminal,
    window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    let Some(worktree) = this.active_path() else {
        return;
    };
    this.close_focused_terminal(&worktree, window, cx);
}

pub fn toggle_terminal(
    this: &mut ClaudhubApp,
    _: &ToggleTerminal,
    window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.toggle_terminal_panel(window, cx);
}

pub fn next_terminal(
    this: &mut ClaudhubApp,
    _: &NextTerminal,
    window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    let Some(worktree) = this.active_path() else {
        return;
    };
    this.step_terminal(&worktree, 1, window, cx);
}

pub fn open_settings(
    this: &mut ClaudhubApp,
    _: &OpenSettings,
    window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.open_settings(window, cx);
}

pub fn zoom_in(
    this: &mut ClaudhubApp,
    _: &ZoomIn,
    window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.zoom(1., window, cx);
}

pub fn zoom_out(
    this: &mut ClaudhubApp,
    _: &ZoomOut,
    window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.zoom(-1., window, cx);
}

pub fn zoom_reset(
    this: &mut ClaudhubApp,
    _: &ZoomReset,
    window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.reset_zoom(window, cx);
}

pub fn copy_diff(
    this: &mut ClaudhubApp,
    _: &CopyDiff,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.copy_diff(false, cx);
}

pub fn copy_diff_patch(
    this: &mut ClaudhubApp,
    _: &CopyDiffPatch,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.copy_diff(true, cx);
}

pub fn select_whole_diff(
    this: &mut ClaudhubApp,
    _: &SelectWholeDiff,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.select_whole_diff(cx);
}

pub fn previous_line(
    this: &mut ClaudhubApp,
    _: &PreviousLine,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.step_diff_row(-1, false, cx);
}

pub fn next_line(
    this: &mut ClaudhubApp,
    _: &NextLine,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.step_diff_row(1, false, cx);
}

pub fn extend_up(
    this: &mut ClaudhubApp,
    _: &ExtendUp,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.step_diff_row(-1, true, cx);
}

pub fn extend_down(
    this: &mut ClaudhubApp,
    _: &ExtendDown,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.step_diff_row(1, true, cx);
}

pub fn previous_hunk(
    this: &mut ClaudhubApp,
    _: &PreviousHunk,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.step_diff_hunk(-1, cx);
}

pub fn next_hunk(
    this: &mut ClaudhubApp,
    _: &NextHunk,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.step_diff_hunk(1, cx);
}

pub fn previous_file(
    this: &mut ClaudhubApp,
    _: &PreviousFile,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.step_file(-1, cx);
}

pub fn next_file(
    this: &mut ClaudhubApp,
    _: &NextFile,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.step_file(1, cx);
}

pub fn toggle_diff_split(
    this: &mut ClaudhubApp,
    _: &ToggleDiffSplit,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.toggle_diff_split(cx);
}

pub fn toggle_whole_file(
    this: &mut ClaudhubApp,
    _: &ToggleWholeFile,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.toggle_whole_file(cx);
}

pub fn commit(
    this: &mut ClaudhubApp,
    _: &Commit,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.commit(false, cx);
}

pub fn annotate_selection(
    this: &mut ClaudhubApp,
    _: &AnnotateSelection,
    window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.annotate_selection(window, cx);
}

pub fn ask_agent(
    this: &mut ClaudhubApp,
    _: &AskAgent,
    window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.ask_about_selection(window, cx);
}

pub fn send_notes(
    this: &mut ClaudhubApp,
    _: &SendNotes,
    window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.send_notes(None, window, cx);
}

pub fn save_file(
    this: &mut ClaudhubApp,
    _: &SaveFile,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.save_file(cx);
}

pub fn explorer_up(
    this: &mut ClaudhubApp,
    _: &ExplorerUp,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.step_project_cursor(-1, cx);
}

pub fn explorer_down(
    this: &mut ClaudhubApp,
    _: &ExplorerDown,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.step_project_cursor(1, cx);
}

pub fn explorer_left(
    this: &mut ClaudhubApp,
    _: &ExplorerLeft,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.fold_project_cursor(false, cx);
}

pub fn explorer_right(
    this: &mut ClaudhubApp,
    _: &ExplorerRight,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.fold_project_cursor(true, cx);
}

pub fn explorer_open(
    this: &mut ClaudhubApp,
    _: &ExplorerOpen,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.activate_project_cursor(cx);
}

pub fn db_up(
    this: &mut ClaudhubApp,
    _: &DbUp,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.db_step_cursor(-1, cx);
}

pub fn db_down(
    this: &mut ClaudhubApp,
    _: &DbDown,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.db_step_cursor(1, cx);
}

pub fn db_left(
    this: &mut ClaudhubApp,
    _: &DbLeft,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.db_fold_cursor(false, cx);
}

pub fn db_right(
    this: &mut ClaudhubApp,
    _: &DbRight,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.db_fold_cursor(true, cx);
}

pub fn db_open(
    this: &mut ClaudhubApp,
    _: &DbOpen,
    window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.db_open_cursor(window, cx);
}

pub fn run_db_query(
    this: &mut ClaudhubApp,
    _: &RunDbQuery,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.run_db_query(cx);
}

pub fn go_to_workspace(
    this: &mut ClaudhubApp,
    action: &GoToWorkspace,
    window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    if let Some(workspace) = crate::ui::workspace::Workspace::ALL
        .get(action.index)
        .copied()
    {
        this.enter_workspace(workspace, window, cx);
    }
}

pub fn copy_db_result(
    this: &mut ClaudhubApp,
    _: &CopyDbResult,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.copy_db_result(cx);
}

pub fn select_whole_result(
    this: &mut ClaudhubApp,
    _: &SelectWholeResult,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.select_whole_db_result(cx);
}

pub fn export_db_csv(
    this: &mut ClaudhubApp,
    _: &ExportDbCsv,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.export_db_csv(cx);
}

pub fn find(
    this: &mut ClaudhubApp,
    _: &Find,
    window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.open_find(window, cx);
}

pub fn close_find(
    this: &mut ClaudhubApp,
    _: &CloseFind,
    window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.close_find(window, cx);
}

pub fn find_next(
    this: &mut ClaudhubApp,
    _: &FindNext,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.find_step(1, cx);
}

pub fn find_previous(
    this: &mut ClaudhubApp,
    _: &FindPrevious,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.find_step(-1, cx);
}

pub fn show_shortcuts(
    this: &mut ClaudhubApp,
    _: &ShowShortcuts,
    window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.open_shortcuts(window, cx);
}

pub fn toggle_sidebar(
    this: &mut ClaudhubApp,
    _: &ToggleSidebar,
    window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.toggle_sidebar(window, cx);
}

pub fn previous_terminal(
    this: &mut ClaudhubApp,
    _: &PreviousTerminal,
    window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    let Some(worktree) = this.active_path() else {
        return;
    };
    this.step_terminal(&worktree, -1, window, cx);
}

pub fn select_worktree(
    this: &mut ClaudhubApp,
    action: &SelectWorktree,
    window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.select_worktree_at(action.index, window, cx);
}

pub fn fetch(
    this: &mut ClaudhubApp,
    _: &Fetch,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.fetch(cx);
}

pub fn pull(
    this: &mut ClaudhubApp,
    _: &Pull,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.pull(cx);
}

pub fn push(
    this: &mut ClaudhubApp,
    _: &Push,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.push(cx);
}

pub fn toggle_stage(
    this: &mut ClaudhubApp,
    _: &ToggleStage,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.toggle_stage_of_open_file(cx);
}

pub fn toggle_review_tree(
    this: &mut ClaudhubApp,
    _: &ToggleReviewTree,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.toggle_review_tree(cx);
}

pub fn diff_start(
    this: &mut ClaudhubApp,
    _: &DiffStart,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.jump_diff(crate::ui::diff_view::Jump::Start, cx);
}

pub fn diff_end(
    this: &mut ClaudhubApp,
    _: &DiffEnd,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.jump_diff(crate::ui::diff_view::Jump::End, cx);
}

pub fn diff_page_up(
    this: &mut ClaudhubApp,
    _: &DiffPageUp,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.jump_diff(crate::ui::diff_view::Jump::PageUp, cx);
}

pub fn diff_page_down(
    this: &mut ClaudhubApp,
    _: &DiffPageDown,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.jump_diff(crate::ui::diff_view::Jump::PageDown, cx);
}

pub fn close_editor(
    this: &mut ClaudhubApp,
    _: &CloseEditor,
    window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.close_editor(window, cx);
}

pub fn explorer_home(
    this: &mut ClaudhubApp,
    _: &ExplorerHome,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.jump_project_cursor(false, cx);
}

pub fn explorer_end(
    this: &mut ClaudhubApp,
    _: &ExplorerEnd,
    _window: &mut Window,
    cx: &mut gpui::Context<ClaudhubApp>,
) {
    this.jump_project_cursor(true, cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels() -> Labels {
        Labels {
            shift: "Shift".into(),
            escape: "Esc".into(),
            enter: "Enter".into(),
            home: "Home".into(),
            end: "Fin".into(),
        }
    }

    #[test]
    fn a_binding_reads_the_way_a_keyboard_is_labelled() {
        let l = labels();
        assert_eq!(pretty("f5", &l), "F5");
        assert_eq!(
            pretty("secondary-shift-e", &l),
            format!("{SECONDARY}+Shift+E")
        );
        assert_eq!(pretty("shift-up", &l), "Shift+↑");
        assert_eq!(pretty("escape", &l), "Esc");
        assert_eq!(pretty("pagedown", &l), "Page ↓");
        // The dash here is the key, not a modifier separator.
        assert_eq!(pretty("secondary--", &l), format!("{SECONDARY}+-"));
        assert_eq!(pretty("secondary-,", &l), format!("{SECONDARY}+,"));
    }

    /// vim's notation is part of what the user already knows: translating it
    /// into "Shift+G" would be teaching them something else.
    #[test]
    fn a_vim_binding_reads_the_way_vim_writes_it() {
        assert_eq!(vim_pretty("g g"), "gg");
        assert_eq!(vim_pretty("shift-g"), "G");
        assert_eq!(vim_pretty("] c"), "]c");
        assert_eq!(vim_pretty("j"), "j");
        assert_eq!(vim_pretty("secondary-d"), format!("{SECONDARY}+D"));
    }

    #[test]
    fn a_run_of_numbered_keys_is_shown_as_a_range() {
        let mut keys = "Ctrl+1".to_string();
        for n in 2..=9 {
            keys = merge(&keys, &format!("Ctrl+{n}"));
        }
        assert_eq!(keys, "Ctrl+1 … Ctrl+9");
        // Two unrelated keys stay two ways of doing it.
        assert_eq!(merge("F5", "Ctrl+R"), "F5 / Ctrl+R");
    }

    /// `KeyBinding::new` **panics** on a key it cannot read, and `init` runs at
    /// startup: a typo in the table would not show up any other way than by
    /// launching Claudhub.
    #[test]
    fn every_keystroke_parses() {
        let none = Overrides::new();
        assert_eq!(standard_bindings(&none).len(), STANDARD.len());
        assert_eq!(vim_bindings(&none).len(), VIM.len());
    }

    /// The label's key is a **variable**, not a literal: if `tr!` could not
    /// translate them that way, the help would show `shortcut-refresh` instead
    /// of the text, and every other test would still pass.
    #[test]
    fn the_sheet_is_translated_and_not_a_list_of_keys() {
        let sections = sheet(true, &Overrides::new());
        assert!(!sections.is_empty());
        for section in &sections {
            assert!(!section.title.starts_with("shortcut-"), "{}", section.title);
            for row in &section.rows {
                assert!(!row.label.starts_with("shortcut-"), "{}", row.label);
                assert!(!row.keys.is_empty());
            }
        }
        // With the mode off, no vim key is offered.
        let plain = sheet(false, &Overrides::new());
        let keys: Vec<&str> = plain
            .iter()
            .flat_map(|s| s.rows.iter().map(|r| r.keys.as_str()))
            .collect();
        assert!(!keys.iter().any(|k| k.contains("gg")), "{keys:?}");
    }

    /// A binding whose help had no text would show as its key, which no review
    /// catches.
    #[test]
    fn every_label_exists_in_both_catalogs() {
        const EN: &str = include_str!("../../assets/i18n/en.json");
        const FR: &str = include_str!("../../assets/i18n/fr.json");
        let keys = |json: &str| -> std::collections::BTreeSet<String> {
            let value: serde_json::Value = serde_json::from_str(json).unwrap();
            value.as_object().unwrap().keys().cloned().collect()
        };
        let (en, fr) = (keys(EN), keys(FR));
        let needed = STANDARD
            .iter()
            .chain(VIM.iter())
            .map(|entry| entry.label)
            .chain(Group::ORDER.iter().map(|group| group.key()));
        for key in needed {
            assert!(en.contains(key), "\"{key}\" is missing from en.json");
            assert!(fr.contains(key), "\"{key}\" is missing from fr.json");
        }
    }

    /// What a capture writes has to be what the table would have written:
    /// otherwise the same key would read one way when we declare it and another
    /// when the user presses it.
    #[test]
    fn a_pressed_key_is_written_the_way_the_table_writes_it() {
        let round_trip = |keys: &str| {
            let stroke = gpui::Keystroke::parse(keys).expect("a readable keystroke");
            stroke_syntax(&stroke)
        };
        assert_eq!(
            round_trip("secondary-shift-p").as_deref(),
            Some("secondary-shift-p")
        );
        assert_eq!(round_trip("alt-1").as_deref(), Some("alt-1"));
        assert_eq!(round_trip("f5").as_deref(), Some("f5"));
        assert_eq!(round_trip("j").as_deref(), Some("j"));
        // A capital arrives as Shift plus the lower-case letter, which is how
        // the vim table writes `G`.
        assert_eq!(round_trip("shift-g").as_deref(), Some("shift-g"));
        // A modifier alone is not a shortcut: the capture waits.
        assert_eq!(round_trip("ctrl").as_deref(), None);
        // And whatever comes out is installable.
        for keys in ["secondary-shift-p", "alt-1", "f5", "j", "shift-g"] {
            assert!(valid_keys(&round_trip(keys).unwrap()));
        }
    }

    /// The id is what `settings.json` carries: two bindings sharing one would
    /// customise each other, and the file would be read by nobody's rule.
    #[test]
    fn every_binding_has_an_id_of_its_own() {
        let mut seen = std::collections::HashSet::new();
        for entry in STANDARD.iter().chain(VIM.iter()) {
            let id = entry.id();
            assert!(seen.insert(id.clone()), "\"{id}\" is declared twice");
        }
    }

    /// A customised binding replaces ours; an empty one switches it off. Both
    /// go through `KeyBinding::new`, which panics on what it cannot read — the
    /// whole point of `valid_keys`.
    #[test]
    fn a_customised_key_replaces_the_one_it_names() {
        let entry = STANDARD
            .iter()
            .find(|entry| entry.keys == "f5")
            .expect("the refresh binding");
        let mut overrides = Overrides::new();
        overrides.insert(entry.id(), "f9".into());
        assert_eq!(entry.effective(&overrides), "f9");
        assert_eq!(standard_bindings(&overrides).len(), STANDARD.len());

        // Switched off: one binding fewer, and no panic.
        overrides.insert(entry.id(), String::new());
        assert_eq!(standard_bindings(&overrides).len(), STANDARD.len() - 1);

        // Unreadable: skipped and logged rather than a window that panics at
        // startup.
        overrides.insert(entry.id(), "ctrl-nonsense".into());
        assert!(!valid_keys("ctrl-nonsense"));
        assert_eq!(standard_bindings(&overrides).len(), STANDARD.len() - 1);

        assert!(valid_keys("secondary-shift-p"));
        assert!(valid_keys("g g"));
        // Empty is not invalid: it is a binding one has turned off.
        assert!(valid_keys(""));
    }

    /// Two different bindings on the same keys and the same predicate would be
    /// settled by declaration order, which is never what was meant.
    #[test]
    fn no_two_bindings_share_keys_within_a_table() {
        for table in [STANDARD, VIM] {
            let mut seen = std::collections::HashSet::new();
            for entry in table {
                // The worktrees' digits share their label, never their keys.
                assert!(
                    seen.insert((entry.keys, entry.predicate)),
                    "\"{}\" is declared twice under the same predicate",
                    entry.keys
                );
            }
        }
    }
}
