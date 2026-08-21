//! Raccourcis clavier.
//!
//! Un terminal a besoin de presque toutes les combinaisons : Ctrl+C, Ctrl+D,
//! Ctrl+L appartiennent au programme qui tourne dedans, pas à Claudhub. Les
//! raccourcis de l'application passent donc par la touche système
//! (`secondary-`, c'est-à-dire Ctrl sous Linux et Windows, Cmd sous macOS).
//!
//! Ce qui ne suffit pas : sous Linux, `secondary` **est** Ctrl, et une liaison
//! sur `secondary-r` prend le Ctrl+R du shell — la recherche dans
//! l'historique — sans rien dire. D'où deux prédicats et non un : ce qui
//! s'écrit avec une seule lettre (`WINDOW_PREDICATE`) laisse le terminal
//! tranquille, ce qui demande Maj ou une touche de fonction
//! (`PREDICATE`) vaut partout. Les terminaux eux-mêmes ont fixé cette
//! convention : Ctrl+Maj+C pour copier, parce que Ctrl+C est pris.
//!
//! **Une seule table décrit chaque liaison** (`table!`), et c'est d'elle que
//! sortent à la fois `bind_keys` et la fenêtre d'aide. Deux listes auraient
//! divergé au premier ajout, et une aide qui ment sur les touches est pire
//! qu'une absence d'aide.

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

/// Aller au n-ième écran.
///
/// Une action *avec une donnée* plutôt que quatre actions, comme pour les
/// worktrees : `Alt+1` à `Alt+4` font la même chose à un indice près.
#[derive(Clone, PartialEq, Debug, Default, gpui::Action)]
#[action(namespace = claudhub, no_json)]
pub struct GoToWorkspace {
    pub index: usize,
}

/// Aller au n-ième worktree de la barre latérale.
///
/// Une action *avec une donnée* plutôt que neuf actions : `Ctrl+1` à `Ctrl+9`
/// font la même chose à un indice près, et neuf gestionnaires identiques ne
/// diraient rien de plus.
#[derive(Clone, PartialEq, Debug, Default, gpui::Action)]
#[action(namespace = claudhub, no_json)]
pub struct SelectWorktree {
    pub index: usize,
}

/// Prédicat des liaisons. Les couches de gpui-component (dialogue, menu,
/// popover) sont exclues : un raccourci qui se déclenche derrière un dialogue
/// agit sur un état que l'utilisateur ne regarde pas.
///
/// À ne pas confondre avec `context()` : ceci est une *expression*, évaluée
/// contre la pile de contextes du nœud focalisé, et elle n'a de sens que dans
/// `KeyBinding::new`. La passer à `key_context` fait boucler le parseur.
const PREDICATE: &str = "Claudhub && !Dialog && !PopupMenu && !Popover";

/// Prédicat de la validation d'un commit.
///
/// `Ctrl+Entrée` est aussi la touche qui lance une requête dans toutes les
/// consoles SQL qu'on a déjà sous les doigts. Les deux ne peuvent pas coexister
/// sur la même touche sans que l'une prenne l'autre, et c'est la console qui
/// gagne quand on écrit dedans : elle est plus profonde dans la pile de
/// contextes, mais l'exclusion est écrite plutôt que déduite — une résolution
/// par profondeur est exactement le genre de chose qu'on ne relit pas.
const COMMIT_PREDICATE: &str = "Claudhub && !Dialog && !PopupMenu && !Popover && !ClaudhubQuery";

/// Prédicat de ce qui s'écrit avec la touche système et **une seule lettre**.
///
/// Sous Linux, `secondary-s` *est* Ctrl+S, c'est-à-dire XOFF, et `secondary-r`
/// est la recherche arrière du shell. Une liaison qui vaudrait aussi dans le
/// terminal les lui prendrait en silence — et l'agent qui tourne dedans est
/// justement ce qu'on est venu piloter.
const WINDOW_PREDICATE: &str = "Claudhub && !Dialog && !PopupMenu && !Popover && !ClaudhubTerminal";

/// Prédicat de la copie depuis le diff.
///
/// `Ctrl+C` appartient d'abord à qui a le focus : le champ de message de commit
/// a sa propre copie, et le terminal transmet la touche au programme qui
/// tourne. Sans ces deux exclusions, copier une ligne saisie dans le message de
/// commit rendrait le diff à la place.
const COPY_PREDICATE: &str = "Claudhub && !Dialog && !PopupMenu && !Popover && !Input \
     && !ClaudhubTerminal && !ClaudhubQuery";

/// Prédicat de la copie depuis la grille de résultats.
///
/// La console occupe la place du diff : `Ctrl+C` y copie une cellule ou le
/// résultat, jamais le fichier relu — d'où l'exclusion réciproque dans
/// `COPY_PREDICATE`. L'éditeur de requête, lui, garde la sienne, comme le
/// champ de message de commit.
const QUERY_COPY_PREDICATE: &str = "ClaudhubQuery && !Input && !PopupMenu && !Popover";

/// Prédicat de la navigation au clavier.
///
/// Les flèches nues sont les seules touches de Claudhub qui ne passent pas par la
/// touche système, et c'est ce qui les rend délicates : elles appartiennent à
/// qui a le focus. Un champ de saisie déplace son curseur, un terminal les
/// transmet au programme, un menu change d'entrée — ces trois-là sont donc
/// exclus, comme pour la copie.
///
/// L'explorateur en est exclu à son tour : ses flèches lui appartiennent — on
/// y parcourt une arborescence, pas un diff — et deux jeux de liaisons sur la
/// même touche ne se départageraient pas.
const NAVIGATION_PREDICATE: &str = "Claudhub && !Dialog && !PopupMenu && !Popover && !Input \
     && !ClaudhubTerminal && !ClaudhubExplorer && !ClaudhubDb";

/// Prédicat de la navigation en mode vim.
///
/// `ClaudhubVim` est **sur le même nœud** que `Claudhub` — la vue racine — et
/// ce n'est pas un détail de style : `depth_of` évalue chaque identifiant
/// contre un seul niveau de la pile de contextes, si bien que deux
/// identifiants déclarés à deux profondeurs différentes ne se rencontrent
/// jamais dans un `&&`.
const VIM_PREDICATE: &str = "Claudhub && ClaudhubVim && !Dialog && !PopupMenu && !Popover \
     && !Input && !ClaudhubTerminal && !ClaudhubExplorer && !ClaudhubDb";

/// Le contexte que la vue racine déclare. Des identifiants, pas un prédicat :
/// c'est le nom auquel `PREDICATE` se réfère.
///
/// `ClaudhubVim` s'y ajoute quand le mode vim est actif, et cela suffit à
/// l'allumer ou à l'éteindre : le contexte est recalculé à chaque rendu, alors
/// que les liaisons sont posées une fois pour toutes au démarrage.
pub fn context(vim: bool) -> KeyContext {
    let mut context = KeyContext::default();
    context.add("Claudhub");
    if vim {
        context.add("ClaudhubVim");
    }
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

/// Contexte déclaré par une barre de recherche.
///
/// `Échap` la ferme, et n'a rien à fermer ailleurs : la lier globalement
/// ferait d'une touche d'annulation universelle le geste d'un panneau.
const FIND_PREDICATE: &str = "ClaudhubFind";

pub fn find_context() -> KeyContext {
    let mut context = KeyContext::default();
    context.add("ClaudhubFind");
    context
}

/// Contexte déclaré par l'arbre de l'explorateur.
///
/// Les flèches y parcourent une arborescence : haut et bas d'une ligne à
/// l'autre, droite pour déplier, gauche pour replier ou remonter au dossier
/// parent. Ce sont celles de PhpStorm, et de tout explorateur.
const EXPLORER_PREDICATE: &str = "ClaudhubExplorer";

/// Les mêmes en mode vim. `ClaudhubVim` doit être déclaré **par l'arbre
/// lui-même** et non par la racine : voir `VIM_PREDICATE`.
const VIM_EXPLORER_PREDICATE: &str = "ClaudhubExplorer && ClaudhubVim";

/// Contexte déclaré par l'arbre des bases.
///
/// Les mêmes flèches que l'explorateur de projet, sur un autre arbre : celui
/// qui a le focus les prend. Sans ce contexte, elles appartiendraient à la
/// relecture du diff, et parcourir un schéma ferait défiler le code d'à côté.
const DB_PREDICATE: &str = "ClaudhubDb";

/// Les mêmes en mode vim. `ClaudhubVim` doit être déclaré **par l'arbre
/// lui-même** : voir `VIM_PREDICATE`.
const VIM_DB_PREDICATE: &str = "ClaudhubDb && ClaudhubVim";

pub fn db_context(vim: bool) -> KeyContext {
    let mut context = KeyContext::default();
    context.add("ClaudhubDb");
    if vim {
        context.add("ClaudhubVim");
    }
    context
}

/// Contexte déclaré par la console SQL.
///
/// `Ctrl+Entrée` y lance la requête plutôt que de valider un commit ; c'est la
/// convention de toutes les consoles SQL.
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

/// Les familles de l'aide, dans l'ordre où elle les affiche.
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

    /// La clé i18n du titre. La clé et non le texte : un test vérifie que
    /// toutes celles de ce module existent dans les deux catalogues, et il ne
    /// peut le faire que sur des clés.
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

/// Une liaison, telle que l'aide la montre.
///
/// Le même enregistrement sert à `bind_keys` : c'est la seule façon d'être sûr
/// que l'aide dise ce que le clavier fait.
pub struct Entry {
    pub keys: &'static str,
    pub group: Group,
    /// Clé i18n de la description.
    pub label: &'static str,
    /// Gardé pour ce qu'il vaut au test : deux liaisons peuvent porter les
    /// mêmes touches — `Entrée` ouvre un fichier dans l'explorateur et va à
    /// l'occurrence suivante dans une recherche — à condition que leurs
    /// prédicats ne se rencontrent pas.
    #[cfg_attr(not(test), allow(dead_code))]
    pub predicate: &'static str,
}

/// Déclare une famille de liaisons : les touches d'un côté, l'aide de l'autre,
/// écrites une seule fois.
macro_rules! table {
    ($entries:ident, $bind:ident, [
        $($group:ident $keys:literal => $action:expr, $predicate:expr, $label:literal;)*
    ]) => {
        static $entries: &[Entry] = &[$(
            Entry {
                keys: $keys,
                group: Group::$group,
                label: $label,
                predicate: $predicate,
            },
        )*];

        fn $bind() -> Vec<KeyBinding> {
            vec![$(
                KeyBinding::new($keys, $action, Some($predicate)),
            )*]
        }
    };
}

table!(STANDARD, standard_bindings, [
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

    // ── Le dépôt ────────────────────────────────────────────────────────────
    // Avec Maj, donc valables jusque dans le terminal : ces trois-là partent
    // sur le réseau et ne dépendent pas de ce qu'on regarde.
    Repository "secondary-shift-r" => Fetch, PREDICATE, "shortcut-fetch";
    Repository "secondary-shift-u" => Pull, PREDICATE, "shortcut-pull";
    Repository "secondary-shift-p" => Push, PREDICATE, "shortcut-push";
    Repository "secondary-enter" => Commit, COMMIT_PREDICATE, "shortcut-commit";

    // ── La relecture ────────────────────────────────────────────────────────
    // Les flèches nues vont d'une modification à la suivante — c'est le geste
    // de la relecture, les lignes de contexte entre deux hunks n'ayant rien à
    // montrer — et débordent sur le fichier voisin une fois le dernier hunk
    // passé. La touche système descend à la ligne, Maj étend la sélection.
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
    // Copier le code relu, et sa variante qui garde les marqueurs de patch.
    Review "secondary-c" => CopyDiff, COPY_PREDICATE, "shortcut-copy";
    Review "secondary-shift-c" => CopyDiffPatch, COPY_PREDICATE, "shortcut-copy-patch";
    Review "secondary-a" => SelectWholeDiff, COPY_PREDICATE, "shortcut-select-all";
    // Annoter et demander partagent le prédicat de la copie : ils partent d'une
    // sélection dans le diff, et n'ont rien à faire quand c'est un champ de
    // saisie ou un terminal qui a le focus.
    Review "secondary-shift-n" => AnnotateSelection, COPY_PREDICATE, "shortcut-annotate";
    Review "secondary-shift-k" => AskAgent, COPY_PREDICATE, "shortcut-ask";
    Review "secondary-shift-e" => SendNotes, PREDICATE, "shortcut-send-notes";
    Review "secondary-shift-s" => ToggleDiffSplit, PREDICATE, "shortcut-split";
    Review "secondary-shift-f" => ToggleWholeFile, PREDICATE, "shortcut-whole-file";
    Review "secondary-shift-i" => ToggleStage, PREDICATE, "shortcut-stage";
    Review "secondary-shift-l" => ToggleReviewTree, PREDICATE, "shortcut-review-tree";
    // Enregistrer et fermer visent l'éditeur intégré ; dans le terminal, Ctrl+S
    // est XOFF et Ctrl+W efface un mot.
    Review "secondary-s" => SaveFile, WINDOW_PREDICATE, "shortcut-save";
    Review "secondary-w" => CloseEditor, WINDOW_PREDICATE, "shortcut-close-editor";

    // ── L'explorateur ───────────────────────────────────────────────────────
    Explorer "up" => ExplorerUp, EXPLORER_PREDICATE, "shortcut-explorer-up";
    Explorer "down" => ExplorerDown, EXPLORER_PREDICATE, "shortcut-explorer-down";
    Explorer "left" => ExplorerLeft, EXPLORER_PREDICATE, "shortcut-explorer-collapse";
    Explorer "right" => ExplorerRight, EXPLORER_PREDICATE, "shortcut-explorer-expand";
    Explorer "home" => ExplorerHome, EXPLORER_PREDICATE, "shortcut-explorer-first";
    Explorer "end" => ExplorerEnd, EXPLORER_PREDICATE, "shortcut-explorer-last";
    Explorer "enter" => ExplorerOpen, EXPLORER_PREDICATE, "shortcut-explorer-open";

    // ── Les bases ───────────────────────────────────────────────────────────
    // Le même jeu que l'explorateur, sur un autre arbre : c'est celui qui a le
    // focus qui les prend.
    Database "up" => DbUp, DB_PREDICATE, "shortcut-db-up";
    Database "down" => DbDown, DB_PREDICATE, "shortcut-db-down";
    Database "left" => DbLeft, DB_PREDICATE, "shortcut-db-collapse";
    Database "right" => DbRight, DB_PREDICATE, "shortcut-db-expand";
    Database "enter" => DbOpen, DB_PREDICATE, "shortcut-db-open";
    Database "secondary-enter" => RunDbQuery, QUERY_PREDICATE, "shortcut-db-run";
    Database "secondary-c" => CopyDbResult, QUERY_COPY_PREDICATE, "shortcut-db-copy";
    Database "secondary-a" => SelectWholeResult, QUERY_COPY_PREDICATE, "shortcut-db-select-all";
    Database "secondary-shift-e" => ExportDbCsv, QUERY_PREDICATE, "shortcut-db-export";

    // ── La recherche ────────────────────────────────────────────────────────
    // `Ctrl+F` cherche dans le panneau où le dernier clic a eu lieu. Il est
    // exclu du terminal et des champs de saisie, qui ont chacun la leur.
    Search "secondary-f" => Find, COPY_PREDICATE, "shortcut-find";
    Search "secondary-g" => FindNext, WINDOW_PREDICATE, "shortcut-find-next";
    Search "enter" => FindNext, FIND_PREDICATE, "shortcut-find-next";
    Search "secondary-shift-g" => FindPrevious, PREDICATE, "shortcut-find-previous";
    Search "shift-enter" => FindPrevious, FIND_PREDICATE, "shortcut-find-previous";
    Search "escape" => CloseFind, FIND_PREDICATE, "shortcut-close-find";

    // ── Les terminaux ───────────────────────────────────────────────────────
    Terminal "secondary-shift-t" => NewTerminal, PREDICATE, "shortcut-new-terminal";
    Terminal "secondary-shift-w" => CloseTerminal, PREDICATE, "shortcut-close-terminal";
    Terminal "secondary-`" => ToggleTerminal, PREDICATE, "shortcut-toggle-terminal";
    // La même chose sous une touche qu'on trouve sans regarder. Une lettre
    // avec la touche système, donc hors du terminal (`WINDOW_PREDICATE`) : là,
    // `Ctrl+T` appartient au programme qui tourne. C'est l'accent grave qui
    // sert à le refermer quand on y a le focus.
    Terminal "secondary-t" => ToggleTerminal, WINDOW_PREDICATE, "shortcut-toggle-terminal";
    Terminal "secondary-tab" => NextTerminal, PREDICATE, "shortcut-next-terminal";
    Terminal "secondary-shift-tab" => PreviousTerminal, PREDICATE, "shortcut-previous-terminal";
    // Les conventions des terminaux : la touche système *avec* Maj, parce que
    // `Ctrl+C` et `Ctrl+V` nus appartiennent au programme.
    Terminal "secondary-shift-c" => CopySelection, TERMINAL_PREDICATE, "shortcut-terminal-copy";
    Terminal "secondary-shift-v" => PasteClipboard, TERMINAL_PREDICATE, "shortcut-terminal-paste";
    Terminal "secondary-shift-a" => SelectAllText, TERMINAL_PREDICATE, "shortcut-terminal-select-all";
]);

table!(VIM, vim_bindings, [
    // Pas de modes ni d'opérateurs : Claudhub n'est pas un éditeur, et son
    // éditeur intégré appartient à gpui-component. Ce qui est repris, c'est la
    // main gauche sur la rangée de repos pour parcourir un diff — ce qu'un
    // relecteur fait mille fois par revue.
    Review "j" => NextLine, VIM_PREDICATE, "shortcut-next-line";
    Review "k" => PreviousLine, VIM_PREDICATE, "shortcut-previous-line";
    // La convention de vim-gitgutter et de fugitive pour aller d'un bloc
    // modifié au suivant.
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
    // Les liaisons vim sont posées **toujours**, et c'est le contexte
    // `ClaudhubVim` qui les allume : `bind_keys` s'appelle une fois au
    // démarrage, alors que le réglage se change en cours de route.
    cx.bind_keys(standard_bindings());
    cx.bind_keys(vim_bindings());
}

/// Une famille de l'aide, prête à afficher.
pub struct Section {
    pub title: SharedString,
    pub rows: Vec<Row>,
}

pub struct Row {
    pub keys: String,
    pub label: SharedString,
}

/// Les raccourcis, groupés, tels que la fenêtre d'aide les montre.
///
/// Les liaisons vim n'y figurent que quand le mode l'est : les afficher
/// éteintes ferait une liste deux fois plus longue dont la moitié ne marche
/// pas.
pub fn sheet(vim: bool) -> Vec<Section> {
    let labels = Labels::current();
    let mut sections = Vec::new();
    for group in Group::ORDER {
        let mut rows: Vec<Row> = Vec::new();
        // Deux liaisons pour un même geste — F5 et Ctrl+R, Ctrl+1 à Ctrl+9,
        // la flèche et son équivalent vim — tiennent sur une ligne : c'est le
        // geste qu'on cherche dans cette liste, pas la touche.
        let mut push = |entry: &Entry, keys: String| {
            let label = tr!(entry.label);
            match rows.iter_mut().find(|row| row.label == label) {
                Some(row) => row.keys = merge(&row.keys, &keys),
                None => rows.push(Row { keys, label }),
            }
        };
        for entry in STANDARD.iter().filter(|e| e.group == group) {
            push(entry, pretty(entry.keys, &labels));
        }
        if vim {
            for entry in VIM.iter().filter(|e| e.group == group) {
                push(entry, vim_pretty(entry.keys));
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

/// Les mots que la lecture d'une touche emprunte à la langue.
///
/// Passés en argument plutôt que lus par `tr!` au fond de la fonction : c'est
/// ce qui rend `pretty` libre et testable, et le catalogue n'est pas chargé
/// dans un test unitaire.
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

/// Le nom de la touche système, tel que son clavier l'écrit.
const SECONDARY: &str = if cfg!(target_os = "macos") {
    "⌘"
} else {
    "Ctrl"
};

/// Une liaison gpui rendue lisible : `secondary-shift-e` → `Ctrl+Maj+E`.
pub fn pretty(keys: &str, labels: &Labels) -> String {
    keys.split(' ')
        .map(|stroke| {
            let mut parts: Vec<String> = Vec::new();
            let mut rest = stroke;
            // Le nom de la touche peut être un tiret (`secondary--`) : c'est
            // le *dernier* segment, jamais un modificateur.
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
        // Les touches de fonction s'écrivent en majuscule, les lettres aussi,
        // et le reste — `,` `-` `` ` `` — tel quel.
        other => other.to_uppercase(),
    }
}

/// Une liaison vim rendue **comme vim l'écrit** : `g g` → `gg`, `shift-g` →
/// `G`, `] c` → `]c`.
///
/// Traduire ces touches comme les autres donnerait « Maj+G » là où tout ce que
/// l'utilisateur connaît dit « G » : la notation fait partie de ce qu'il sait
/// déjà, et la remplacer serait lui apprendre autre chose.
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

/// Réunit deux façons de faire le même geste sur une seule ligne.
///
/// Une suite de touches numérotées — `Ctrl+1` … `Ctrl+9` — s'écrit comme une
/// plage ; deux touches sans rapport s'écrivent l'une ou l'autre.
fn merge(current: &str, next: &str) -> String {
    if let Some(start) = current.split(['…', '/']).next().map(str::trim) {
        if consecutive(start, next) || current.contains('…') {
            let first = current.split('…').next().unwrap_or(current).trim();
            return format!("{first} … {next}");
        }
    }
    format!("{current} / {next}")
}

/// Deux touches qui ne diffèrent que par un chiffre qui se suit.
///
/// Le découpage se fait sur le **caractère** et non sur l'octet : une touche
/// s'écrit couramment `Ctrl+↓`, et couper un pas avant la fin d'une flèche
/// est une panique.
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
    let group = this.terminal_group(&worktree, window, cx);
    group.update(cx, |group, cx| {
        group.open(crate::ui::terminal_view::Launch::shell(), window, cx);
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
    let group = this.terminal_group(&worktree, window, cx);
    group.update(cx, |group, cx| group.previous(window, cx));
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
        // Deux touches sans rapport restent deux façons de faire.
        assert_eq!(merge("F5", "Ctrl+R"), "F5 / Ctrl+R");
    }

    /// `KeyBinding::new` **panique** sur une touche qu'elle ne sait pas lire,
    /// et `init` tourne au démarrage : une faute de frappe dans la table ne se
    /// verrait pas autrement qu'en lançant Claudhub.
    #[test]
    fn every_keystroke_parses() {
        assert_eq!(standard_bindings().len(), STANDARD.len());
        assert_eq!(vim_bindings().len(), VIM.len());
    }

    /// La clé du libellé est une **variable**, pas un littéral : si `tr!` ne
    /// savait pas les traduire ainsi, l'aide afficherait `shortcut-refresh` à
    /// la place du texte, et tous les autres tests passeraient quand même.
    #[test]
    fn the_sheet_is_translated_and_not_a_list_of_keys() {
        let sections = sheet(true);
        assert!(!sections.is_empty());
        for section in &sections {
            assert!(!section.title.starts_with("shortcut-"), "{}", section.title);
            for row in &section.rows {
                assert!(!row.label.starts_with("shortcut-"), "{}", row.label);
                assert!(!row.keys.is_empty());
            }
        }
        // Le mode éteint, aucune touche de vim n'est proposée.
        let plain = sheet(false);
        let keys: Vec<&str> = plain
            .iter()
            .flat_map(|s| s.rows.iter().map(|r| r.keys.as_str()))
            .collect();
        assert!(!keys.iter().any(|k| k.contains("gg")), "{keys:?}");
    }

    /// Une liaison dont l'aide n'aurait pas le texte s'afficherait sous la
    /// forme de sa clé, ce qu'aucune relecture ne rattrape.
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
