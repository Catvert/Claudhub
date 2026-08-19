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

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("f5", Refresh, Some(PREDICATE)),
        KeyBinding::new("secondary-r", Refresh, Some(PREDICATE)),
        KeyBinding::new("secondary-shift-t", NewTerminal, Some(PREDICATE)),
        KeyBinding::new("secondary-shift-w", CloseTerminal, Some(PREDICATE)),
        KeyBinding::new("secondary-`", ToggleTerminal, Some(PREDICATE)),
        KeyBinding::new("secondary-tab", NextTerminal, Some(PREDICATE)),
        KeyBinding::new("secondary-enter", Commit, Some(PREDICATE)),
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

pub fn commit(
    this: &mut PerchApp,
    _: &Commit,
    _window: &mut Window,
    cx: &mut gpui::Context<PerchApp>,
) {
    this.commit(false, cx);
}
