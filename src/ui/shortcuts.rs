//! Raccourcis clavier.
//!
//! Un terminal a besoin de presque toutes les combinaisons : Ctrl+C, Ctrl+D,
//! Ctrl+L appartiennent au programme qui tourne dedans, pas à Perch. Les
//! raccourcis de l'application passent donc tous par la touche système
//! (`secondary-`, c'est-à-dire Ctrl sous Linux et Windows, Cmd sous macOS),
//! que `key_bytes` refuse justement de transmettre au pty.

use gpui::{actions, App, KeyBinding, KeyContext, Window};

use crate::ui::app::PerchApp;

actions!(
    perch,
    [
        ToggleHistory,
        ShowWorking,
        ShowBranch,
        Refresh,
        NewTerminal,
        CloseTerminal,
        ToggleTerminal,
        NextTerminal,
        Commit
    ]
);

/// Prédicat des liaisons. Les couches de gpui-component (dialogue, menu,
/// popover) sont exclues : un raccourci qui se déclenche derrière un dialogue
/// agit sur un état que l'utilisateur ne regarde pas.
///
/// À ne pas confondre avec `context()` : ceci est une *expression*, évaluée
/// contre la pile de contextes du nœud focalisé, et elle n'a de sens que dans
/// `KeyBinding::new`. La passer à `key_context` fait boucler le parseur.
const PREDICATE: &str = "Perch && !Dialog && !PopupMenu && !Popover";

/// Le contexte que la vue racine déclare. Un simple identifiant : c'est le
/// nom auquel `PREDICATE` se réfère.
pub fn context() -> KeyContext {
    let mut context = KeyContext::default();
    context.add("Perch");
    context
}

// Actions traitées par le terminal qui a le focus, et non par la fenêtre.
// Les noms portent leur objet (`CopySelection` plutôt que `Copy`) : une action
// nommée `Copy` entrerait en collision avec le trait du même nom, que tout
// module Rust a dans son périmètre.
actions!(
    perch_terminal,
    [CopySelection, PasteClipboard, SelectAllText]
);

/// Contexte déclaré par une vue de terminal. Les trois raccourcis ci-dessous
/// n'existent que là : `Ctrl+Maj+C` ailleurs dans l'interface n'aurait rien à
/// copier, et `Ctrl+C` tout court appartient au programme qui tourne.
const TERMINAL_PREDICATE: &str = "PerchTerminal";

pub fn terminal_context() -> KeyContext {
    let mut context = KeyContext::default();
    context.add("PerchTerminal");
    context
}

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("f5", Refresh, Some(PREDICATE)),
        KeyBinding::new("secondary-r", Refresh, Some(PREDICATE)),
        KeyBinding::new("secondary-shift-t", NewTerminal, Some(PREDICATE)),
        KeyBinding::new("secondary-shift-w", CloseTerminal, Some(PREDICATE)),
        KeyBinding::new("secondary-`", ToggleTerminal, Some(PREDICATE)),
        KeyBinding::new("secondary-tab", NextTerminal, Some(PREDICATE)),
        // Les quatre domaines de revue, dans l'ordre des onglets.
        KeyBinding::new("secondary-h", ToggleHistory, Some(PREDICATE)),
        KeyBinding::new("secondary-1", ShowWorking, Some(PREDICATE)),
        KeyBinding::new("secondary-2", ShowBranch, Some(PREDICATE)),
        KeyBinding::new("secondary-enter", Commit, Some(PREDICATE)),
        // Les conventions des terminaux : la touche système *avec* Maj, parce
        // que `Ctrl+C` et `Ctrl+V` nus appartiennent au programme.
        KeyBinding::new("secondary-shift-c", CopySelection, Some(TERMINAL_PREDICATE)),
        KeyBinding::new(
            "secondary-shift-v",
            PasteClipboard,
            Some(TERMINAL_PREDICATE),
        ),
        KeyBinding::new("secondary-shift-a", SelectAllText, Some(TERMINAL_PREDICATE)),
    ]);
}

pub fn refresh(
    this: &mut PerchApp,
    _: &Refresh,
    _window: &mut Window,
    cx: &mut gpui::Context<PerchApp>,
) {
    this.refresh_active(cx);
}

pub fn new_terminal(
    this: &mut PerchApp,
    _: &NewTerminal,
    window: &mut Window,
    cx: &mut gpui::Context<PerchApp>,
) {
    let Some(worktree) = this.active_path() else {
        return;
    };
    let group = this.terminal_group(&worktree, window, cx);
    group.update(cx, |group, cx| {
        group.open(None, crate::tr!("terminal-shell"), window, cx);
    });
    this.show_terminal_panel(cx);
}

pub fn close_terminal(
    this: &mut PerchApp,
    _: &CloseTerminal,
    window: &mut Window,
    cx: &mut gpui::Context<PerchApp>,
) {
    let Some(worktree) = this.active_path() else {
        return;
    };
    let group = this.terminal_group(&worktree, window, cx);
    group.update(cx, |group, cx| {
        let index = group.active_index();
        group.close(index, window, cx);
    });
}

pub fn toggle_terminal(
    this: &mut PerchApp,
    _: &ToggleTerminal,
    _window: &mut Window,
    cx: &mut gpui::Context<PerchApp>,
) {
    this.toggle_terminal_panel(cx);
}

pub fn next_terminal(
    this: &mut PerchApp,
    _: &NextTerminal,
    window: &mut Window,
    cx: &mut gpui::Context<PerchApp>,
) {
    let Some(worktree) = this.active_path() else {
        return;
    };
    let group = this.terminal_group(&worktree, window, cx);
    group.update(cx, |group, cx| group.next(window, cx));
}

pub fn toggle_history(
    this: &mut PerchApp,
    _: &ToggleHistory,
    _window: &mut Window,
    cx: &mut gpui::Context<PerchApp>,
) {
    this.toggle_history_panel(cx);
}

pub fn show_working(
    this: &mut PerchApp,
    _: &ShowWorking,
    _window: &mut Window,
    cx: &mut gpui::Context<PerchApp>,
) {
    this.set_range(crate::git::DiffRange::Working, cx);
}

/// Sans base connue, il n'y a rien à comparer : le raccourci ne fait rien,
/// comme l'onglet correspondant est inactif.
pub fn show_branch(
    this: &mut PerchApp,
    _: &ShowBranch,
    _window: &mut Window,
    cx: &mut gpui::Context<PerchApp>,
) {
    if let Some(base) = this.review_base() {
        this.set_range(crate::git::DiffRange::Branch { base }, cx);
    }
}

pub fn commit(
    this: &mut PerchApp,
    _: &Commit,
    _window: &mut Window,
    cx: &mut gpui::Context<PerchApp>,
) {
    this.commit(false, cx);
}
