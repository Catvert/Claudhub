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
//! **L'édition reste légère.** Retouche courte ici, vrai travail dans
//! l'éditeur externe de son choix : Claudhub ne devient pas un IDE, et
//! `external_editor` est ce qui rend ce partage praticable.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{div, prelude::*, px, uniform_list, Context, Entity, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    menu::{ContextMenuExt, PopupMenuItem},
    v_flex, ActiveTheme, Selectable, Sizable, WindowExt,
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
}

impl Default for Explorer {
    fn default() -> Self {
        Self {
            files: Vec::new(),
            rows: Rc::new(Vec::new()),
            collapsed: std::collections::HashSet::new(),
            pending: false,
            ignored: false,
        }
    }
}

impl Explorer {
    fn rebuild(&mut self) {
        self.rows = Rc::new(tree::build(&self.files, &self.collapsed));
    }
}

/// Un fichier ouvert dans l'éditeur intégré.
pub struct Editing {
    pub worktree: PathBuf,
    pub path: PathBuf,
    /// L'entité de saisie, créée **une fois** à l'ouverture du fichier :
    /// recréée dans un rendu, elle perdrait curseur et sélection à la première
    /// frappe.
    pub input: Entity<InputState>,
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
            InputState::new(window, cx)
                .code_editor(language)
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
                .confirm()
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
        if Settings::global(cx).external_editor.trim().is_empty() {
            self.announce(tr!("editor-none-configured"), cx);
            return;
        }
        self.git.send(Cmd::OpenExternal {
            worktree,
            path,
            line,
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
        _window: &mut Window,
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
        let scroll = self.files_scroll.clone();
        let Some(explorer) = self.explorers.get(&worktree) else {
            return div().into_any_element();
        };
        let rows = explorer.rows.clone();
        let files = Rc::new(explorer.files.clone());
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
        let colors = (cx.theme().muted_foreground, cx.theme().accent);

        let bar = h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(icon("folder").xsmall())
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .text_color(colors.0)
                    .child(tr!("files-count", { count: count })),
            )
            .child(
                Button::new("files-ignored")
                    .ghost()
                    .xsmall()
                    .icon(icon(if ignored { "eye" } else { "eye-off" }))
                    .selected(ignored)
                    .tooltip(tr!("files-show-ignored"))
                    .on_click(cx.listener(|this, _, _window, cx| this.toggle_ignored_files(cx))),
            )
            .child(
                Button::new("files-new")
                    .ghost()
                    .xsmall()
                    .icon(icon("plus"))
                    .tooltip(tr!("files-new-file"))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.prompt_new_path(false, window, cx)),
                    ),
            );

        v_flex()
            .size_full()
            .child(bar)
            .child(
                div().flex_1().min_h_0().child(crate::ui::scroll::vertical(
                    "project-files-bar",
                    &scroll,
                    uniform_list("project-files", count, move |visible, _window, cx| {
                        visible
                            .map(|ix| {
                                render_row(
                                    &rows,
                                    &files,
                                    ix,
                                    &status,
                                    open.as_deref(),
                                    colors,
                                    &entity,
                                    cx,
                                )
                            })
                            .collect::<Vec<_>>()
                    })
                    .size_full()
                    .track_scroll(scroll.clone()),
                )),
            )
            .into_any_element()
    }

    /// Demande un chemin et crée le fichier ou le dossier.
    fn prompt_new_path(&mut self, directory: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.open_text_dialog(
            if directory {
                tr!("files-new-dir")
            } else {
                tr!("files-new-file")
            },
            tr!("files-path-placeholder"),
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
                .confirm()
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
                .child(div().flex_1().min_h_0().child(Input::new(&input).h_full())),
        )
    }
}

/// Une ligne de l'explorateur : un dossier repliable ou un fichier.
#[allow(clippy::too_many_arguments)]
fn render_row(
    rows: &Rc<Vec<tree::Entry>>,
    files: &Rc<Vec<PathBuf>>,
    index: usize,
    status: &Rc<std::collections::HashMap<PathBuf, crate::git::StatusCode>>,
    open: Option<&Path>,
    colors: (gpui::Hsla, gpui::Hsla),
    entity: &Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let Some(entry) = rows.get(index) else {
        return div().into_any_element();
    };
    let (muted, accent) = colors;
    match entry {
        tree::Entry::Dir {
            path,
            label,
            depth,
            collapsed,
            ..
        } => {
            let (path, entity) = (path.clone(), entity.clone());
            h_flex()
                .id(("dir", index))
                .py_1()
                .pr_2()
                .pl(px(8. + *depth as f32 * 12.))
                .gap_1()
                .items_center()
                .cursor_pointer()
                .hover(|s| s.bg(accent.opacity(0.4)))
                .on_click(move |_, _window, cx| {
                    entity.update(cx, |this, cx| this.toggle_project_dir(path.clone(), cx));
                })
                .child(
                    icon(if *collapsed {
                        "chevron-right"
                    } else {
                        "chevron-down"
                    })
                    .xsmall(),
                )
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .text_sm()
                        .child(SharedString::from(label.clone())),
                )
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
            let selected = open == Some(path.as_path());
            let (for_open, for_menu) = (path.clone(), path.clone());
            let (open_entity, menu_entity) = (entity.clone(), entity.clone());
            h_flex()
                .id(("file", index))
                .py_1()
                .pr_2()
                .pl(px(20. + *depth as f32 * 12.))
                .gap_1()
                .items_center()
                .cursor_pointer()
                .when(selected, |el| el.bg(accent))
                .hover(|s| s.bg(accent.opacity(0.4)))
                .on_click(move |_, _window, cx| {
                    open_entity.update(cx, |this, cx| this.open_in_editor(for_open.clone(), cx));
                })
                .child(crate::ui::file_icons::file_icon(&path, cx))
                .child(
                    div()
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
                            .text_color(muted)
                            .child(SharedString::new_static(code.letter())),
                    )
                })
                .context_menu(move |menu, _window, _cx| {
                    let path = for_menu.clone();
                    let (rename, delete, external, copy) = (
                        menu_entity.clone(),
                        menu_entity.clone(),
                        menu_entity.clone(),
                        menu_entity.clone(),
                    );
                    let (p1, p2, p3, p4) = (path.clone(), path.clone(), path.clone(), path.clone());
                    menu.item(PopupMenuItem::new(tr!("editor-external")).on_click(
                        move |_, _window, cx| {
                            external.update(cx, |this, cx| this.open_externally(p1.clone(), 1, cx));
                        },
                    ))
                    .item(PopupMenuItem::new(tr!("action-copy-path")).on_click(
                        move |_, _window, cx| {
                            copy.update(cx, |this, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                    p2.display().to_string(),
                                ));
                                this.announce(tr!("copy-path-done"), cx);
                            });
                        },
                    ))
                    .separator()
                    .item(
                        PopupMenuItem::new(tr!("files-rename")).on_click(move |_, window, cx| {
                            rename
                                .update(cx, |this, cx| this.prompt_rename(p3.clone(), window, cx));
                        }),
                    )
                    .item(
                        PopupMenuItem::new(tr!("files-delete")).on_click(move |_, window, cx| {
                            delete
                                .update(cx, |this, cx| this.confirm_delete(p4.clone(), window, cx));
                        }),
                    )
                })
                .into_any_element()
        }
    }
}
