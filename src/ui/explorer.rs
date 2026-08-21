//! L'explorateur de projet, et la retouche d'un fichier.
//!
//! **L'arbre vient d'un seul appel git** (`ls-files --cached --others
//! --exclude-standard`), pas d'un parcours de disque : un projet Laravel a
//! quarante mille répertoires, et les ouvrir un par un coûterait un appel
//! système chacun pour arriver aux sept cents qui portent du code.
//!
//! **L'arbre est construit une fois**, à l'arrivée de la liste et à chaque
//! repli, et rangé derrière un `Rc`. Contrairement à la liste de revue — qui
//! compte des centaines d'entrées — celle-ci en compte des dizaines de
//! milliers : la reconstruire à chaque frame ferait tomber l'interface.
//!
//! **Il se parcourt au clavier**, comme celui de PhpStorm : haut et bas d'une
//! ligne à l'autre de la liste *affichée*, droite pour déplier, gauche pour
//! replier ou remonter au dossier parent, Entrée pour ouvrir. D'où un contexte
//! clavier à lui (`ClaudhubExplorer`), les flèches nues appartenant sinon à la
//! relecture du diff.
//!
//! **Le curseur est un chemin, pas un indice.** L'arbre se reconstruit à
//! chaque repli, à chaque frappe de recherche et à chaque relecture de la
//! liste : un indice y désignerait une autre ligne d'une fois sur l'autre.
//!
//! **Ouvert et sous le curseur sont deux choses**, et se voient différemment :
//! on parcourt l'arbre au clavier sans quitter le fichier qu'on relit.
//!
//! **L'édition reste légère.** Retouche courte ici, vrai travail dans
//! l'éditeur externe de son choix : Claudhub ne devient pas un IDE, et
//! `external_editor` est ce qui rend ce partage praticable.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{div, prelude::*, px, uniform_list, Context, Entity, Pixels, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::{Editor, EditorState},
    menu::{ContextMenuExt, DropdownMenu, PopupMenuItem},
    v_flex, ActiveTheme, Sizable, WindowExt,
};

use crate::files;
use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::settings::Settings;
use crate::ui::theme::status_color;
use crate::ui::tree;

/// Les fichiers d'un worktree, et l'arbre qu'on en tire.
pub struct Explorer {
    /// La liste plate, telle que git la rend : c'est elle la référence, et
    /// l'arbre n'en est qu'un affichage.
    pub files: Vec<PathBuf>,
    /// L'arbre affiché, reconstruit à chaque repli et jamais dans un rendu.
    pub rows: Rc<Vec<tree::Entry>>,
    pub collapsed: std::collections::HashSet<PathBuf>,
    /// Une demande est partie et n'est pas revenue : sans ce garde, chaque
    /// frame du panneau relancerait `ls-files`.
    pub pending: bool,
    /// Les fichiers ignorés étaient-ils demandés, pour savoir quand relire.
    pub ignored: bool,
    /// La recherche pour laquelle `rows` a été construit. Comparée au rendu :
    /// c'est le prix de n'avoir personne à prévenir quand elle change.
    pub query: String,
    /// La ligne sur laquelle le clavier travaille — un fichier ou un dossier.
    ///
    /// Un **chemin** et non un indice : l'arbre se reconstruit à chaque repli,
    /// à chaque frappe de recherche et à chaque relecture de la liste, et un
    /// indice y désignerait une autre ligne d'une fois sur l'autre.
    pub cursor: Option<PathBuf>,
}

impl Default for Explorer {
    fn default() -> Self {
        Self {
            files: Vec::new(),
            rows: Rc::new(Vec::new()),
            collapsed: std::collections::HashSet::new(),
            pending: false,
            ignored: false,
            query: String::new(),
            cursor: None,
        }
    }
}

impl Explorer {
    fn rebuild(&mut self) {
        // Pendant une recherche, les replis sont ignorés et l'arbre est réduit
        // à ce qui correspond : un fichier trouvé dans un dossier fermé ne se
        // verrait pas, et la recherche paraîtrait n'avoir rien trouvé.
        let keep: Option<Vec<usize>> = (!self.query.trim().is_empty()).then(|| {
            self.files
                .iter()
                .enumerate()
                .filter(|(_, path)| crate::ui::find::matches(&self.query, &path.to_string_lossy()))
                .map(|(index, _)| index)
                .collect()
        });
        let open = std::collections::HashSet::new();
        let collapsed = if keep.is_some() {
            &open
        } else {
            &self.collapsed
        };
        let rows = tree::build_subset(&self.files, keep.as_deref(), collapsed);
        self.rows = Rc::new(rows);
    }

    /// Le chemin d'une entrée affichée, dossier ou fichier.
    fn path_at(&self, index: usize) -> Option<PathBuf> {
        match self.rows.get(index)? {
            tree::Entry::Dir { path, .. } => Some(path.clone()),
            tree::Entry::Leaf { index, .. } => self.files.get(*index).cloned(),
        }
    }

    /// Où se trouve un chemin dans la liste affichée, s'il y est encore.
    fn row_of(&self, wanted: &Path) -> Option<usize> {
        (0..self.rows.len()).find(|index| self.path_at(*index).as_deref() == Some(wanted))
    }

    fn is_dir(&self, index: usize) -> bool {
        matches!(self.rows.get(index), Some(tree::Entry::Dir { .. }))
    }

    /// Ouvre tous les dossiers qui mènent à un chemin.
    ///
    /// Retirer chaque ancêtre suffit, y compris avec les chaînes fusionnées :
    /// `app/Http/Livewire/Forms` tient sur une ligne mais reste un ancêtre du
    /// fichier qu'elle contient.
    fn reveal(&mut self, path: &Path) {
        let mut changed = false;
        for ancestor in path.ancestors().skip(1) {
            changed |= self.collapsed.remove(ancestor);
        }
        if changed {
            self.rebuild();
        }
    }

    /// Replie tout ce qui est ouvert au premier niveau et en dessous.
    fn collapse_all(&mut self) {
        // Tous les dossiers, et non seulement ceux qu'on voit : ce qu'un
        // dossier fermé cache doit l'être aussi quand on le rouvrira.
        for path in &self.files {
            for ancestor in path.ancestors().skip(1) {
                if !ancestor.as_os_str().is_empty() {
                    self.collapsed.insert(ancestor.to_path_buf());
                }
            }
        }
        self.rebuild();
    }

    /// Déplie tout un sous-arbre.
    fn expand_under(&mut self, root: &Path) {
        self.collapsed
            .retain(|path| !path.starts_with(root) && path != root);
        self.rebuild();
    }

    /// Replie tout un sous-arbre, sa racine comprise.
    fn collapse_under(&mut self, root: &Path) {
        for path in &self.files {
            if !path.starts_with(root) {
                continue;
            }
            for ancestor in path.ancestors().skip(1) {
                if ancestor.starts_with(root) {
                    self.collapsed.insert(ancestor.to_path_buf());
                }
            }
        }
        self.collapsed.insert(root.to_path_buf());
        self.rebuild();
    }
}

/// Un fichier ouvert dans l'éditeur intégré.
pub struct Editing {
    pub worktree: PathBuf,
    pub path: PathBuf,
    /// L'entité de saisie, créée **une fois** à l'ouverture du fichier :
    /// recréée dans un rendu, elle perdrait curseur et sélection à la première
    /// frappe.
    pub input: Entity<EditorState>,
    /// Empreinte du contenu lu, ce qui permet de refuser d'écraser le travail
    /// d'un agent.
    pub hash: u64,
    /// Ce qui est à l'écran diffère de ce qui est sur le disque.
    pub dirty: bool,
}

impl ClaudhubApp {
    // — L'arbre ————————————————————————————————————————————————

    fn explorer(&mut self) -> Option<&mut Explorer> {
        let worktree = self.active.clone()?;
        Some(self.explorers.entry(worktree).or_default())
    }

    /// Demande la liste des fichiers, si elle manque ou si le réglage a changé.
    ///
    /// Appelée au rendu du panneau : c'est lui qui sait ce qu'il affiche, et
    /// charger la liste d'avance coûterait une commande pour un onglet que
    /// personne n'ouvrira.
    pub(super) fn ensure_project_files(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let ignored = Settings::global(cx).show_ignored_files;
        let explorer = self.explorers.entry(worktree.clone()).or_default();
        if explorer.pending || (!explorer.files.is_empty() && explorer.ignored == ignored) {
            return;
        }
        explorer.pending = true;
        explorer.ignored = ignored;
        self.git.send(Cmd::ListFiles { worktree, ignored });
    }

    pub(super) fn project_files_arrived(&mut self, worktree: PathBuf, files: Vec<PathBuf>) {
        let explorer = self.explorers.entry(worktree).or_default();
        explorer.pending = false;
        explorer.files = files;
        explorer.rebuild();
    }

    pub(super) fn toggle_project_dir(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(explorer) = self.explorer() else {
            return;
        };
        if !explorer.collapsed.remove(&path) {
            explorer.collapsed.insert(path);
        }
        explorer.rebuild();
        cx.notify();
    }

    /// Amène la ligne du curseur sous les yeux, sans faire sauter la liste
    /// quand elle y est déjà.
    fn reveal_cursor(&mut self) {
        let Some(explorer) = self.explorer() else {
            return;
        };
        let Some(index) = explorer
            .cursor
            .clone()
            .and_then(|path| explorer.row_of(&path))
        else {
            return;
        };
        self.files_scroll
            .scroll_to_item(index, gpui::ScrollStrategy::Top);
    }

    /// Monte ou descend d'une ligne dans l'arborescence affichée.
    ///
    /// La liste affichée, replis compris : c'est celle que l'œil suit, et
    /// descendre dans un dossier fermé mènerait à des lignes invisibles.
    pub(super) fn step_project_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(explorer) = self.explorer() else {
            return;
        };
        let count = explorer.rows.len();
        if count == 0 {
            return;
        }
        let current = explorer
            .cursor
            .clone()
            .and_then(|path| explorer.row_of(&path))
            .map(|index| index as isize);
        // Sans curseur, la première flèche entre par le bout vers lequel elle
        // pointe, comme la relecture d'un diff.
        let next = match current {
            Some(index) => (index + delta).clamp(0, count as isize - 1),
            None if delta > 0 => 0,
            None => count as isize - 1,
        } as usize;
        explorer.cursor = explorer.path_at(next);
        self.reveal_cursor();
        cx.notify();
    }

    /// Porte le curseur au premier ou au dernier de la liste affichée.
    pub(super) fn jump_project_cursor(&mut self, last: bool, cx: &mut Context<Self>) {
        let Some(explorer) = self.explorer() else {
            return;
        };
        let count = explorer.rows.len();
        if count == 0 {
            return;
        }
        explorer.cursor = explorer.path_at(if last { count - 1 } else { 0 });
        self.reveal_cursor();
        cx.notify();
    }

    /// Déplie ou replie au curseur.
    ///
    /// Sur un fichier, la flèche gauche remonte au dossier parent et la droite
    /// descend d'une ligne : c'est ce que fait tout explorateur, et une touche
    /// inerte se lit comme une touche cassée.
    pub(super) fn fold_project_cursor(&mut self, open: bool, cx: &mut Context<Self>) {
        let Some(explorer) = self.explorer() else {
            return;
        };
        let Some(path) = explorer.cursor.clone() else {
            return self.step_project_cursor(if open { 1 } else { -1 }, cx);
        };
        let Some(index) = explorer.row_of(&path) else {
            return;
        };
        if explorer.is_dir(index) {
            let is_collapsed = explorer.collapsed.contains(&path);
            if open == is_collapsed {
                if open {
                    explorer.collapsed.remove(&path);
                } else {
                    explorer.collapsed.insert(path);
                }
                explorer.rebuild();
                cx.notify();
                return;
            }
        }
        if open {
            self.step_project_cursor(1, cx);
            return;
        }
        // Remonter au dossier qui contient la ligne : le premier ancêtre qui
        // soit lui-même affiché, les chaînes fusionnées sautant des niveaux.
        let parent = path
            .ancestors()
            .skip(1)
            .find(|ancestor| explorer.row_of(ancestor).is_some())
            .map(Path::to_path_buf);
        if parent.is_some() {
            explorer.cursor = parent;
            self.reveal_cursor();
            cx.notify();
        }
    }

    /// Entrée : ouvre le fichier, ou replie le dossier.
    pub(super) fn activate_project_cursor(&mut self, cx: &mut Context<Self>) {
        let Some(explorer) = self.explorer() else {
            return;
        };
        let Some(path) = explorer.cursor.clone() else {
            return;
        };
        let Some(index) = explorer.row_of(&path) else {
            return;
        };
        if explorer.is_dir(index) {
            self.toggle_project_dir(path, cx);
        } else {
            self.open_in_editor(path, cx);
        }
    }

    /// Montre dans l'arbre le fichier qu'on est en train de regarder.
    ///
    /// Le geste « scroll from source » de PhpStorm : on relit un diff, on veut
    /// voir où le fichier vit. Il déplie ce qu'il faut pour l'atteindre, et
    /// n'est **pas** automatique — une liste qui saute toute seule à chaque
    /// clic dans la revue est un mouvement de trop.
    pub(super) fn reveal_open_file(&mut self, cx: &mut Context<Self>) {
        let path = self
            .editing
            .as_ref()
            .map(|editing| editing.path.clone())
            .or_else(|| {
                self.active_review()
                    .and_then(|state| state.selected.clone())
            });
        let Some(path) = path else {
            return;
        };
        let Some(explorer) = self.explorer() else {
            return;
        };
        explorer.reveal(&path);
        explorer.cursor = Some(path);
        self.reveal_cursor();
        cx.notify();
    }

    /// Donne le focus à l'arbre et y pose le curseur.
    ///
    /// Sans le focus, la flèche qui suit le clic partirait au diff : les
    /// liaisons se départagent sur le contexte du nœud focalisé, et l'arbre
    /// n'est pas focalisé du seul fait qu'on a cliqué dedans.
    pub(super) fn focus_project_tree(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.explorer_focus.focus(window, cx);
        if let Some(explorer) = self.explorer() {
            explorer.cursor = Some(path);
        }
        cx.notify();
    }

    pub(super) fn expand_project_dir(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if let Some(explorer) = self.explorer() {
            explorer.expand_under(&path);
        }
        cx.notify();
    }

    pub(super) fn collapse_project_dir(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if let Some(explorer) = self.explorer() {
            explorer.collapse_under(&path);
        }
        cx.notify();
    }

    /// Copie le chemin d'une entrée, relatif au worktree ou absolu.
    ///
    /// Les deux servent, et pas aux mêmes choses : le relatif se colle dans
    /// l'invite d'un agent, qui travaille depuis le worktree ; l'absolu dans
    /// un terminal ouvert ailleurs.
    pub(super) fn copy_project_path(
        &mut self,
        path: &Path,
        absolute: bool,
        cx: &mut Context<Self>,
    ) {
        let text = match (absolute, self.active.as_ref()) {
            (true, Some(worktree)) => worktree.join(path).display().to_string(),
            _ => path.display().to_string(),
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.announce(tr!("copy-path-done"), cx);
    }

    pub(super) fn collapse_project_tree(&mut self, cx: &mut Context<Self>) {
        if let Some(explorer) = self.explorer() {
            explorer.collapse_all();
        }
        cx.notify();
    }

    pub(super) fn toggle_ignored_files(&mut self, cx: &mut Context<Self>) {
        Settings::update_global(cx, |s| s.show_ignored_files = !s.show_ignored_files);
        // La liste change d'ordre de grandeur : on la redemande plutôt que de
        // filtrer celle qu'on a, qui n'a jamais vu les fichiers ignorés.
        if let Some(explorer) = self.explorer() {
            explorer.files.clear();
            explorer.rebuild();
        }
        cx.notify();
    }

    // — Lire et écrire ————————————————————————————————————————

    /// Ouvre un fichier dans l'éditeur intégré.
    pub(super) fn open_in_editor(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        self.git.send(Cmd::ReadFile { worktree, path });
        cx.notify();
    }

    /// Reçoit un contenu et installe l'éditeur.
    pub(super) fn file_content_arrived(
        &mut self,
        worktree: PathBuf,
        path: PathBuf,
        content: files::Content,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Le langage se déduit de l'extension, comme pour la coloration d'un
        // diff : c'est la même table, PHP compris.
        let language = crate::ui::highlight::language_for_path(&path).unwrap_or("text");
        let input = cx.new(|cx| {
            // `EditorState` et non `InputState` : la refonte des saisies a
            // séparé les trois modes en trois types — une ligne, du texte
            // multiligne, du code. Les fonctions de code (langage, numéros de
            // ligne, LSP) n'existent que sur le troisième.
            EditorState::new(window, cx)
                .language(language)
                .line_number(true)
                .default_value(content.text)
        });
        // La souscription est posée ici, une fois par fichier ouvert : c'est
        // elle qui allume l'indicateur de modification non enregistrée.
        cx.subscribe(&input, |this, _, event, cx| {
            if !matches!(event, gpui_component::input::InputEvent::Change) {
                return;
            }
            if let Some(editing) = this.editing.as_mut() {
                editing.dirty = true;
            }
            cx.notify();
        })
        .detach();
        self.editing = Some(Editing {
            worktree,
            path,
            input,
            hash: content.hash,
            dirty: false,
        });
        // Un fichier qui s'ouvre appelle l'écran où il s'édite. Le geste vient
        // de l'explorateur — donc de cet écran-là la plupart du temps — mais
        // aussi d'une ligne de diff, et y répondre en silence sur l'écran d'à
        // côté serait un fichier ouvert que personne ne voit.
        self.enter_workspace(crate::ui::workspace::Workspace::Files, window, cx);
        self.set_panel_visible(crate::ui::panels::EditorPanel::NAME, true, cx);
        cx.notify();
    }

    pub(super) fn save_file(&mut self, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.as_ref() else {
            return;
        };
        let content = editing.input.read(cx).value().to_string();
        self.git.send(Cmd::WriteFile {
            worktree: editing.worktree.clone(),
            path: editing.path.clone(),
            content: content.clone(),
            // L'empreinte de ce qu'on avait lu : un agent qui a écrit entre
            // temps fait refuser l'enregistrement plutôt que d'être écrasé.
            expect: Some(editing.hash),
        });
        // L'empreinte suit ce qu'on vient d'envoyer : sans cela, deux
        // enregistrements d'affilée feraient refuser le second, le fichier
        // ayant changé — par nous.
        if let Some(editing) = self.editing.as_mut() {
            editing.hash = files::digest(&content);
            editing.dirty = false;
        }
        cx.notify();
    }

    /// Ferme l'éditeur, en demandant confirmation si le fichier a changé.
    pub(super) fn close_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.as_ref() else {
            return;
        };
        if !editing.dirty {
            self.editing = None;
            cx.notify();
            return;
        }
        let label = SharedString::from(editing.path.display().to_string());
        let entity = cx.entity();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (entity, label) = (entity.clone(), label.clone());
            dialog
                .title(tr!("editor-discard-title"))
                .child(
                    v_flex()
                        .gap_1()
                        .child(div().text_sm().child(label.clone()))
                        .child(div().text_xs().child(tr!("editor-discard-help"))),
                )
                .overlay_closable(false)
                .close_button(false)
                .on_ok(move |_, _window, cx| {
                    entity.update(cx, |this, cx| {
                        this.editing = None;
                        cx.notify();
                    });
                    true
                })
        });
    }

    /// Ouvre un fichier dans l'éditeur externe, à une ligne donnée.
    ///
    /// Le geste existe **depuis une ligne de diff** autant que depuis
    /// l'explorateur : c'est le cas d'usage réel — on relit, quelque chose
    /// cloche, on l'ouvre là où c'est.
    pub(super) fn open_externally(&mut self, path: PathBuf, line: usize, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let editor = Settings::global(cx).external_editor.clone();
        if editor.trim().is_empty() {
            self.announce(tr!("editor-none-configured"), cx);
            return;
        }
        self.git.send(Cmd::OpenExternal {
            worktree,
            path,
            line,
            editor,
        });
        cx.notify();
    }

    /// Ouvre le fichier du diff dans l'éditeur externe, à la ligne
    /// sélectionnée.
    pub(super) fn open_diff_externally(&mut self, cx: &mut Context<Self>) {
        let split = Settings::global(cx).diff_split;
        let Some(state) = self.active_review() else {
            return;
        };
        let Some(path) = state.selected.clone() else {
            return;
        };
        let line = state
            .diff
            .as_ref()
            .zip(state.diff_selection)
            .and_then(|(diff, (anchor, head))| {
                let row = if split {
                    diff.unified_span(anchor, head)?.0
                } else {
                    anchor.min(head)
                };
                let crate::ui::diff_view::Row::Line { hunk, line } = diff.rows.get(row).copied()?
                else {
                    return None;
                };
                let source = diff.file.hunks.get(hunk)?.lines.get(line)?;
                source.new_no.or(source.old_no)
            })
            .unwrap_or(1);
        self.open_externally(path, line, cx);
    }

    fn file_op(&mut self, op: files::Op, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        self.git.send(Cmd::FileOp { worktree, op });
        // La liste est relue : `ls-files` seul sait ce que git suit désormais.
        if let Some(explorer) = self.explorer() {
            explorer.files.clear();
        }
        cx.notify();
    }

    // — Le panneau ——————————————————————————————————————————————

    pub(super) fn render_files(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(worktree) = self.active.clone() else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .child(tr!("no-worktree"))
                .into_any_element();
        };
        self.ensure_project_files(cx);
        let ignored = Settings::global(cx).show_ignored_files;
        let vim = Settings::global(cx).vim_mode;
        let scroll = self.files_scroll.clone();
        let focus = self.explorer_focus.clone();
        let find = self.render_find(crate::ui::find::Pane::Files, cx);
        let query = self.query(crate::ui::find::Pane::Files, cx);
        let bar = self.render_files_bar(&worktree, ignored, cx);
        let Some(explorer) = self.explorers.get_mut(&worktree) else {
            return div().into_any_element();
        };
        if explorer.query != query {
            explorer.query = query;
            explorer.rebuild();
        }
        let explorer = &*explorer;
        let pending = explorer.pending;
        let rows = explorer.rows.clone();
        let files = Rc::new(explorer.files.clone());
        let cursor = explorer.cursor.clone();
        let count = rows.len();
        // Le statut git est déjà là : l'afficher ne coûte qu'une consultation
        // par ligne visible, et c'est ce qui fait la différence entre une
        // liste de fichiers et un explorateur de projet.
        let status: Rc<std::collections::HashMap<PathBuf, crate::git::StatusCode>> = Rc::new(
            self.review
                .get(&worktree)
                .map(|state| {
                    state
                        .status
                        .files
                        .iter()
                        .map(|file| {
                            let code = if file.is_untracked() {
                                crate::git::StatusCode::Untracked
                            } else if !matches!(file.worktree, crate::git::StatusCode::Unmodified) {
                                file.worktree
                            } else {
                                file.index
                            };
                            (file.path.clone(), code)
                        })
                        .collect()
                })
                .unwrap_or_default(),
        );
        let open = self.editing.as_ref().map(|editing| editing.path.clone());
        let entity = cx.entity();
        let look = Look::of(cx);

        // Rien à montrer, et rien en route : c'est un projet vide ou une
        // recherche sans résultat. Pendant le premier `ls-files`, la liste
        // reste blanche — annoncer « aucun fichier » puis les afficher se lit
        // comme un défaut d'affichage.
        if count == 0 && !pending {
            return v_flex()
                .size_full()
                .child(bar)
                .children(find)
                .child(
                    v_flex()
                        .size_full()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .text_color(look.muted)
                        .child(icon("folder"))
                        .child(div().text_sm().child(tr!("files-empty"))),
                )
                .into_any_element();
        }

        v_flex()
            .size_full()
            .child(bar)
            .children(find)
            .child(
                div()
                    .id("project-tree")
                    // Les flèches appartiennent à l'arbre tant qu'il a le
                    // focus : c'est ce contexte-là que leur prédicat lit.
                    .key_context(crate::ui::shortcuts::explorer_context(vim))
                    .track_focus(&focus)
                    .flex_1()
                    .min_h_0()
                    .child(
                        self.scrolled(
                            "project-files-bar",
                            &scroll,
                            crate::ui::motion::Axes::Vertical,
                            window,
                            uniform_list("project-files", count, move |visible, _window, cx| {
                                visible
                                    .map(|ix| {
                                        render_row(
                                            &rows,
                                            &files,
                                            ix,
                                            &status,
                                            open.as_deref(),
                                            cursor.as_deref(),
                                            &look,
                                            &entity,
                                            cx,
                                        )
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .size_full()
                            // Voir `review.rs` : le retrait appartient à la
                            // liste, une marge sur une entrée de
                            // `uniform_list` étant ignorée.
                            .px_1()
                            .track_scroll(&scroll.clone()),
                            cx,
                        ),
                    ),
            )
            .into_any_element()
    }

    /// L'en-tête : le projet, ce qu'il pèse, et les gestes de l'arbre.
    ///
    /// Trois boutons et un menu plutôt que six boutons : le panneau est
    /// étroit par nature — c'est une colonne de noms de fichiers — et ce qui
    /// ne sert qu'une fois de temps en temps n'a pas à y prendre la place de
    /// ce qui sert à chaque relecture.
    fn render_files_bar(
        &mut self,
        worktree: &Path,
        ignored: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let name = worktree
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| worktree.display().to_string());
        let count = self
            .explorers
            .get(worktree)
            .map(|explorer| explorer.files.len())
            .unwrap_or(0);
        let muted = cx.theme().muted_foreground;
        let entity = cx.entity();

        h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(icon("folder-open").xsmall().text_color(muted))
            .child(
                div()
                    .flex_1()
                    .truncate()
                    .text_xs()
                    .child(SharedString::from(name)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(SharedString::from(count.to_string())),
            )
            .child(
                Button::new("files-search")
                    .ghost()
                    .xsmall()
                    .icon(icon("search"))
                    .tooltip(tr!("files-search"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        // Le panneau devient la cible de la recherche : le
                        // bouton est dans son en-tête, et cliquer dedans ne
                        // passe pas forcément par le contenu.
                        this.touch_pane(crate::ui::find::Pane::Files, cx);
                        this.open_find(window, cx);
                    })),
            )
            .child(
                Button::new("files-reveal")
                    .ghost()
                    .xsmall()
                    .icon(icon("crosshair"))
                    .tooltip(tr!("files-reveal"))
                    .on_click(cx.listener(|this, _, _window, cx| this.reveal_open_file(cx))),
            )
            .child(
                Button::new("files-collapse")
                    .ghost()
                    .xsmall()
                    .icon(icon("chevrons-down-up"))
                    .tooltip(tr!("files-collapse-all"))
                    .on_click(cx.listener(|this, _, _window, cx| this.collapse_project_tree(cx))),
            )
            .child(
                Button::new("files-more")
                    .ghost()
                    .xsmall()
                    .icon(icon("ellipsis"))
                    .tooltip(tr!("files-more"))
                    .dropdown_menu(move |menu, _window, _cx| {
                        let (file, dir, hidden) = (entity.clone(), entity.clone(), entity.clone());
                        menu.item(
                            PopupMenuItem::new(tr!("files-new-file"))
                                .icon(icon("file-plus"))
                                .on_click(move |_, window, cx| {
                                    file.update(cx, |this, cx| {
                                        this.prompt_new_path(None, false, window, cx)
                                    });
                                }),
                        )
                        .item(
                            PopupMenuItem::new(tr!("files-new-dir"))
                                .icon(icon("folder-plus"))
                                .on_click(move |_, window, cx| {
                                    dir.update(cx, |this, cx| {
                                        this.prompt_new_path(None, true, window, cx)
                                    });
                                }),
                        )
                        .separator()
                        .item(
                            PopupMenuItem::new(tr!("files-show-ignored"))
                                .icon(icon("eye"))
                                .icon(icon(if ignored { "eye" } else { "eye-off" }))
                                .on_click(move |_, _window, cx| {
                                    hidden.update(cx, |this, cx| this.toggle_ignored_files(cx));
                                }),
                        )
                    }),
            )
    }

    /// Demande un chemin et crée le fichier ou le dossier.
    ///
    /// `parent` préremplit le champ : c'est ce qui fait la différence entre
    /// « nouveau fichier » et « nouveau fichier *ici* », le second étant le
    /// geste qu'on a réellement depuis un clic droit sur un dossier.
    fn prompt_new_path(
        &mut self,
        parent: Option<PathBuf>,
        directory: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let start = parent
            .map(|parent| format!("{}/", parent.display()))
            .unwrap_or_default();
        self.open_text_dialog_with(
            if directory {
                tr!("files-new-dir")
            } else {
                tr!("files-new-file")
            },
            tr!("files-path-placeholder"),
            start,
            window,
            cx,
            move |this, value, _window, cx| {
                let path = PathBuf::from(value.trim());
                if path.as_os_str().is_empty() {
                    return;
                }
                this.file_op(
                    if directory {
                        files::Op::NewDir { path }
                    } else {
                        files::Op::NewFile { path }
                    },
                    cx,
                );
            },
        );
    }

    fn prompt_rename(&mut self, from: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.open_text_dialog(
            tr!("files-rename"),
            SharedString::from(from.display().to_string()),
            window,
            cx,
            move |this, value, _window, cx| {
                let to = PathBuf::from(value.trim());
                if to.as_os_str().is_empty() || to == from {
                    return;
                }
                this.file_op(
                    files::Op::Rename {
                        from: from.clone(),
                        to,
                    },
                    cx,
                );
            },
        );
    }

    /// Confirme avant de supprimer : c'est le seul geste de l'explorateur que
    /// git ne rattrape pas quand le fichier n'est pas suivi.
    fn confirm_delete(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let label = SharedString::from(path.display().to_string());
        let entity = cx.entity();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (entity, path, label) = (entity.clone(), path.clone(), label.clone());
            dialog
                .title(tr!("delete-title"))
                .child(
                    v_flex()
                        .gap_1()
                        .child(div().text_sm().child(label.clone()))
                        .child(div().text_xs().child(tr!("delete-warning"))),
                )
                .overlay_closable(false)
                .close_button(false)
                .on_ok(move |_, _window, cx| {
                    entity.update(cx, |this, cx| {
                        this.file_op(files::Op::Delete { path: path.clone() }, cx)
                    });
                    true
                })
        });
    }

    // — L'éditeur ————————————————————————————————————————————

    /// L'éditeur intégré, quand un fichier y est ouvert.
    ///
    /// Il prend la place du diff plutôt que d'occuper un panneau à lui : on
    /// regarde l'un *ou* l'autre, et deux onglets à faire basculer pour un
    /// geste qui vient de l'explorateur seraient un aller-retour de trop.
    pub(super) fn render_editor(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let editing = self.editing.as_ref()?;
        let (path, dirty, input) = (editing.path.clone(), editing.dirty, editing.input.clone());
        let mono = cx.theme().mono_font_family.clone();
        let label = SharedString::from(path.display().to_string());
        let for_external = path.clone();
        Some(
            v_flex()
                .size_full()
                .child(
                    h_flex()
                        .h(crate::ui::theme::bar_height(cx))
                        .w_full()
                        .px_2()
                        .gap_2()
                        .items_center()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(icon("file-text").xsmall())
                        .child(
                            div()
                                .flex_1()
                                .truncate()
                                .text_sm()
                                .font_family(mono)
                                .child(label),
                        )
                        // Une pastille et non un astérisque dans le titre :
                        // le titre est déjà un chemin tronqué, et un caractère
                        // de plus au bout ne se voit pas.
                        .when(dirty, |el| {
                            el.child(div().size(px(7.)).rounded_full().bg(cx.theme().warning))
                        })
                        .child(
                            Button::new("editor-external")
                                .ghost()
                                .xsmall()
                                .icon(icon("external-link"))
                                .tooltip(tr!("editor-external"))
                                .on_click(cx.listener(move |this, _, _window, cx| {
                                    this.open_externally(for_external.clone(), 1, cx);
                                })),
                        )
                        .child(
                            Button::new("editor-save")
                                .ghost()
                                .xsmall()
                                .icon(icon("save"))
                                .tooltip(tr!("editor-save"))
                                .on_click(cx.listener(|this, _, _window, cx| this.save_file(cx))),
                        )
                        .child(
                            Button::new("editor-close")
                                .ghost()
                                .xsmall()
                                .icon(icon("x"))
                                .tooltip(tr!("editor-close"))
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.close_editor(window, cx)
                                    }),
                                ),
                        ),
                )
                .child(div().flex_1().min_h_0().child(Editor::new(&input).h_full())),
        )
    }
}

/// Ce qui ne dépend pas de la ligne : couleurs et géométrie.
///
/// Lu une fois par frame et non par entrée visible — la fermeture de la liste
/// virtualisée tourne pour chaque ligne à l'écran, animation de molette
/// comprise, et `cx.theme()` emprunte le contexte.
struct Look {
    height: Pixels,
    /// Le rayon du fond d'une ligne. Une ligne survolée ou ouverte est une
    /// pastille posée dans la liste, pas une bande qui la traverse.
    radius: Pixels,
    muted: gpui::Hsla,
    accent: gpui::Hsla,
    /// Le filet vertical d'un niveau d'indentation.
    guide: gpui::Hsla,
    folder: gpui::Hsla,
}

impl Look {
    fn of(cx: &gpui::App) -> Self {
        Self {
            height: crate::ui::theme::row_height(cx),
            radius: cx.theme().radius,
            muted: cx.theme().muted_foreground,
            accent: cx.theme().accent,
            // Assez pâle pour se lire comme une trame et non comme un
            // séparateur : ces filets sont là par dizaines à l'écran.
            guide: cx.theme().border.opacity(0.7),
            folder: cx.theme().muted_foreground,
        }
    }
}

/// Largeur d'un niveau d'indentation, et du filet qui le marque.
const INDENT: f32 = 12.;

/// Les filets verticaux des niveaux parents.
///
/// C'est ce qui rend une arborescence profonde lisible : sans eux, à six
/// niveaux d'indentation — le cas courant sur un projet Laravel — plus rien ne
/// dit à quel dossier une ligne appartient.
fn indent_guides(depth: usize, look: &Look) -> impl IntoIterator<Item = gpui::Div> + use<> {
    let guide = look.guide;
    (0..depth).map(move |_| {
        div()
            .w(px(INDENT))
            .h_full()
            .flex_none()
            .border_l_1()
            .border_color(guide)
    })
}

/// Une ligne de l'explorateur : un dossier repliable ou un fichier.
#[allow(clippy::too_many_arguments)]
fn render_row(
    rows: &Rc<Vec<tree::Entry>>,
    files: &Rc<Vec<PathBuf>>,
    index: usize,
    status: &Rc<std::collections::HashMap<PathBuf, crate::git::StatusCode>>,
    open: Option<&Path>,
    cursor: Option<&Path>,
    look: &Look,
    entity: &Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let Some(entry) = rows.get(index) else {
        return div().into_any_element();
    };
    match entry {
        tree::Entry::Dir {
            path,
            label,
            depth,
            collapsed,
            ..
        } => {
            let at_cursor = cursor == Some(path.as_path());
            let (path, entity) = (path.clone(), entity.clone());
            let (for_click, for_menu) = (path.clone(), path.clone());
            let click = entity.clone();
            h_flex()
                .id(("dir", index))
                .h(look.height)
                .rounded(look.radius)
                .pl_1()
                .pr_2()
                .items_center()
                .cursor_pointer()
                .when(at_cursor, |el| el.bg(look.accent.opacity(0.5)))
                .hover(|s| s.bg(look.accent.opacity(0.4)))
                .on_click(move |_, window, cx| {
                    click.update(cx, |this, cx| {
                        this.focus_project_tree(for_click.clone(), window, cx);
                        this.toggle_project_dir(for_click.clone(), cx);
                    });
                })
                .children(indent_guides(*depth, look))
                .child(
                    icon(if *collapsed {
                        "chevron-right"
                    } else {
                        "chevron-down"
                    })
                    .xsmall()
                    .text_color(look.muted),
                )
                // Le dossier porte son propre glyphe, ouvert ou fermé : le
                // chevron dit l'état du repli, l'icône dit qu'on regarde un
                // dossier — c'est ce qui distingue une arborescence d'une
                // liste indentée.
                .child(
                    icon(if *collapsed { "folder" } else { "folder-open" })
                        .xsmall()
                        .text_color(look.folder),
                )
                .child(
                    div()
                        .pl_1()
                        .flex_1()
                        .truncate()
                        .text_sm()
                        .child(SharedString::from(label.clone())),
                )
                .context_menu(move |menu, _window, _cx| dir_menu(menu, &entity, &for_menu))
                .into_any_element()
        }
        tree::Entry::Leaf { index: leaf, depth } => {
            let Some(path) = files.get(*leaf).cloned() else {
                return div().into_any_element();
            };
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let code = status.get(&path).copied();
            let is_open = open == Some(path.as_path());
            let at_cursor = cursor == Some(path.as_path());
            let (for_open, for_menu) = (path.clone(), path.clone());
            let (open_entity, menu_entity) = (entity.clone(), entity.clone());
            h_flex()
                .id(("file", index))
                .h(look.height)
                .rounded(look.radius)
                .pl_1()
                .pr_2()
                .items_center()
                .cursor_pointer()
                // Ouvert et sous le curseur sont deux choses : on parcourt
                // l'arbre au clavier sans quitter le fichier qu'on relit, et
                // ne montrer que l'un des deux perdrait l'autre.
                .when(is_open, |el| el.bg(look.accent))
                .when(at_cursor && !is_open, |el| el.bg(look.accent.opacity(0.5)))
                .hover(|s| s.bg(look.accent.opacity(0.4)))
                .on_click(move |_, window, cx| {
                    open_entity.update(cx, |this, cx| {
                        this.focus_project_tree(for_open.clone(), window, cx);
                        this.open_in_editor(for_open.clone(), cx);
                    });
                })
                .children(indent_guides(*depth, look))
                // La place du chevron qu'un fichier n'a pas : sans elle, les
                // noms de fichiers et ceux des dossiers ne s'alignent pas.
                .child(div().w(px(14.)).flex_none())
                .child(crate::ui::file_icons::file_icon(&path, cx))
                .child(
                    div()
                        .pl_1()
                        .flex_1()
                        .truncate()
                        .text_sm()
                        .when_some(code, |el, code| el.text_color(status_color(code, cx)))
                        .child(SharedString::from(name)),
                )
                .when_some(code, |el, code| {
                    el.child(
                        div()
                            .text_xs()
                            .text_color(look.muted)
                            .child(SharedString::new_static(code.letter())),
                    )
                })
                .context_menu(move |menu, _window, _cx| file_menu(menu, &menu_entity, &for_menu))
                .into_any_element()
        }
    }
}

/// Le menu d'un dossier : créer dedans, et le déplier ou le replier en bloc.
fn dir_menu(
    menu: gpui_component::menu::PopupMenu,
    entity: &Entity<ClaudhubApp>,
    path: &Path,
) -> gpui_component::menu::PopupMenu {
    let (new_file, new_dir) = (entity.clone(), entity.clone());
    let (expand, collapse, copy) = (entity.clone(), entity.clone(), entity.clone());
    let (p1, p2, p3, p4, p5) = (
        path.to_path_buf(),
        path.to_path_buf(),
        path.to_path_buf(),
        path.to_path_buf(),
        path.to_path_buf(),
    );
    menu.item(
        PopupMenuItem::new(tr!("files-new-here"))
            .icon(icon("file-plus"))
            .on_click(move |_, window, cx| {
                new_file.update(cx, |this, cx| {
                    this.prompt_new_path(Some(p1.clone()), false, window, cx)
                });
            }),
    )
    .item(
        PopupMenuItem::new(tr!("files-new-dir-here"))
            .icon(icon("folder-plus"))
            .on_click(move |_, window, cx| {
                new_dir.update(cx, |this, cx| {
                    this.prompt_new_path(Some(p2.clone()), true, window, cx)
                });
            }),
    )
    .separator()
    .item(
        PopupMenuItem::new(tr!("files-expand-under"))
            .icon(icon("chevrons-up-down"))
            .on_click(move |_, _window, cx| {
                expand.update(cx, |this, cx| this.expand_project_dir(p3.clone(), cx));
            }),
    )
    .item(
        PopupMenuItem::new(tr!("files-collapse-under"))
            .icon(icon("chevrons-down-up"))
            .on_click(move |_, _window, cx| {
                collapse.update(cx, |this, cx| this.collapse_project_dir(p4.clone(), cx));
            }),
    )
    .separator()
    .item(
        PopupMenuItem::new(tr!("action-copy-path"))
            .icon(icon("copy"))
            .on_click(move |_, _window, cx| {
                copy.update(cx, |this, cx| this.copy_project_path(&p5, false, cx));
            }),
    )
}

/// Le menu d'un fichier.
fn file_menu(
    menu: gpui_component::menu::PopupMenu,
    entity: &Entity<ClaudhubApp>,
    path: &Path,
) -> gpui_component::menu::PopupMenu {
    let (external, copy, absolute) = (entity.clone(), entity.clone(), entity.clone());
    let (new_file, rename, delete) = (entity.clone(), entity.clone(), entity.clone());
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let (p1, p2, p3, p4, p5) = (
        path.to_path_buf(),
        path.to_path_buf(),
        path.to_path_buf(),
        path.to_path_buf(),
        path.to_path_buf(),
    );
    menu.item(
        PopupMenuItem::new(tr!("editor-external"))
            .icon(icon("external-link"))
            .on_click(move |_, _window, cx| {
                external.update(cx, |this, cx| this.open_externally(p1.clone(), 1, cx));
            }),
    )
    .separator()
    .item(
        PopupMenuItem::new(tr!("action-copy-path"))
            .icon(icon("copy"))
            .on_click(move |_, _window, cx| {
                copy.update(cx, |this, cx| this.copy_project_path(&p2, false, cx));
            }),
    )
    .item(
        PopupMenuItem::new(tr!("files-copy-absolute"))
            .icon(icon("copy"))
            .on_click(move |_, _window, cx| {
                absolute.update(cx, |this, cx| this.copy_project_path(&p3, true, cx));
            }),
    )
    .separator()
    // « Ici » veut dire dans le dossier du fichier : on clique droit sur un
    // voisin de ce qu'on veut créer, jamais sur le dossier lui-même quand la
    // liste en montre déjà le contenu.
    .item(
        PopupMenuItem::new(tr!("files-new-here"))
            .icon(icon("file-plus"))
            .on_click(move |_, window, cx| {
                new_file.update(cx, |this, cx| {
                    this.prompt_new_path(Some(parent.clone()), false, window, cx)
                });
            }),
    )
    .item(
        PopupMenuItem::new(tr!("files-rename"))
            .icon(icon("pencil"))
            .on_click(move |_, window, cx| {
                rename.update(cx, |this, cx| this.prompt_rename(p4.clone(), window, cx));
            }),
    )
    .item(
        PopupMenuItem::new(tr!("files-delete"))
            .icon(icon("trash-2"))
            .on_click(move |_, window, cx| {
                delete.update(cx, |this, cx| this.confirm_delete(p5.clone(), window, cx));
            }),
    )
}
