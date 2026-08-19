//! Raccourcis clavier.
//!
//! Un terminal a besoin de presque toutes les combinaisons : Ctrl+C, Ctrl+D,
//! Ctrl+L appartiennent au programme qui tourne dedans, pas à Claudhub. Les
//! raccourcis de l'application passent donc tous par la touche système
//! (`secondary-`, c'est-à-dire Ctrl sous Linux et Windows, Cmd sous macOS),
//! que `key_bytes` refuse justement de transmettre au pty.

use gpui::{actions, App, KeyBinding, KeyContext, Window};

use crate::ui::app::ClaudhubApp;

actions!(
    claudhub,
    [
        Refresh,
        NewTerminal,
        CloseTerminal,
        ToggleTerminal,
        NextTerminal,
        Commit,
        OpenSettings,
        ZoomIn,
        ZoomOut,
        ZoomReset,
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
        ToggleDiffSplit,
        ToggleWholeFile
    ]
);

/// Prédicat des liaisons. Les couches de gpui-component (dialogue, menu,
/// popover) sont exclues : un raccourci qui se déclenche derrière un dialogue
/// agit sur un état que l'utilisateur ne regarde pas.
///
/// À ne pas confondre avec `context()` : ceci est une *expression*, évaluée
/// contre la pile de contextes du nœud focalisé, et elle n'a de sens que dans
/// `KeyBinding::new`. La passer à `key_context` fait boucler le parseur.
const PREDICATE: &str = "Claudhub && !Dialog && !PopupMenu && !Popover";

/// Prédicat de la copie depuis le diff.
///
/// `Ctrl+C` appartient d'abord à qui a le focus : le champ de message de commit
/// a sa propre copie, et le terminal transmet la touche au programme qui
/// tourne. Sans ces deux exclusions, copier une ligne saisie dans le message de
/// commit rendrait le diff à la place.
const COPY_PREDICATE: &str =
    "Claudhub && !Dialog && !PopupMenu && !Popover && !Input && !ClaudhubTerminal";

/// Prédicat de la navigation au clavier.
///
/// Les flèches nues sont les seules touches de Claudhub qui ne passent pas par la
/// touche système, et c'est ce qui les rend délicates : elles appartiennent à
/// qui a le focus. Un champ de saisie déplace son curseur, un terminal les
/// transmet au programme, un menu change d'entrée — ces trois-là sont donc
/// exclus, comme pour la copie.
const NAVIGATION_PREDICATE: &str = COPY_PREDICATE;

/// Le contexte que la vue racine déclare. Un simple identifiant : c'est le
/// nom auquel `PREDICATE` se réfère.
pub fn context() -> KeyContext {
    let mut context = KeyContext::default();
    context.add("Claudhub");
    context
}

// Actions traitées par le terminal qui a le focus, et non par la fenêtre.
// Les noms portent leur objet (`CopySelection` plutôt que `Copy`) : une action
// nommée `Copy` entrerait en collision avec le trait du même nom, que tout
// module Rust a dans son périmètre.
actions!(
    claudhub_terminal,
    [CopySelection, PasteClipboard, SelectAllText]
);

/// Contexte déclaré par une vue de terminal. Les trois raccourcis ci-dessous
/// n'existent que là : `Ctrl+Maj+C` ailleurs dans l'interface n'aurait rien à
/// copier, et `Ctrl+C` tout court appartient au programme qui tourne.
const TERMINAL_PREDICATE: &str = "ClaudhubTerminal";

pub fn terminal_context() -> KeyContext {
    let mut context = KeyContext::default();
    context.add("ClaudhubTerminal");
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
        // La convention de tous les éditeurs, y compris sous Linux.
        KeyBinding::new("secondary-,", OpenSettings, Some(PREDICATE)),
        // Le zoom vise la zone qui a le focus : le terminal quand il l'a, les
        // diffs sinon. `secondary-=` autant que `secondary-+` parce que le
        // signe plus demande Maj sur un clavier azerty comme sur un qwerty.
        KeyBinding::new("secondary-=", ZoomIn, Some(PREDICATE)),
        KeyBinding::new("secondary-+", ZoomIn, Some(PREDICATE)),
        KeyBinding::new("secondary--", ZoomOut, Some(PREDICATE)),
        KeyBinding::new("secondary-0", ZoomReset, Some(PREDICATE)),
        // Copier le code relu, et sa variante qui garde les marqueurs de
        // patch.
        KeyBinding::new("secondary-c", CopyDiff, Some(COPY_PREDICATE)),
        KeyBinding::new("secondary-shift-c", CopyDiffPatch, Some(COPY_PREDICATE)),
        KeyBinding::new("secondary-a", SelectWholeDiff, Some(COPY_PREDICATE)),
        // Relire au clavier. Les flèches nues vont d'une modification à la
        // suivante — c'est le geste de la relecture, les lignes de contexte
        // entre deux hunks n'ayant rien à montrer — et débordent sur le
        // fichier voisin une fois le dernier hunk passé. La touche système
        // descend à la ligne, Maj étend la sélection.
        KeyBinding::new("up", PreviousHunk, Some(NAVIGATION_PREDICATE)),
        KeyBinding::new("down", NextHunk, Some(NAVIGATION_PREDICATE)),
        KeyBinding::new("shift-up", ExtendUp, Some(NAVIGATION_PREDICATE)),
        KeyBinding::new("shift-down", ExtendDown, Some(NAVIGATION_PREDICATE)),
        KeyBinding::new("secondary-up", PreviousLine, Some(NAVIGATION_PREDICATE)),
        KeyBinding::new("secondary-down", NextLine, Some(NAVIGATION_PREDICATE)),
        KeyBinding::new("left", PreviousFile, Some(NAVIGATION_PREDICATE)),
        KeyBinding::new("right", NextFile, Some(NAVIGATION_PREDICATE)),
        KeyBinding::new("secondary-shift-s", ToggleDiffSplit, Some(PREDICATE)),
        KeyBinding::new("secondary-shift-f", ToggleWholeFile, Some(PREDICATE)),
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
    let group = this.terminal_group(&worktree, window, cx);
    group.update(cx, |group, cx| {
        group.open(None, crate::tr!("terminal-shell"), window, cx);
    });
    this.show_terminal_panel(window, cx);
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
    let group = this.terminal_group(&worktree, window, cx);
    group.update(cx, |group, cx| {
        let index = group.active_index();
        group.close(index, window, cx);
    });
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
    let group = this.terminal_group(&worktree, window, cx);
    group.update(cx, |group, cx| group.next(window, cx));
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
