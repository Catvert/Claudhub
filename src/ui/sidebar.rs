//! Barre latérale : les dépôts et leurs worktrees.
//!
//! C'est le sélecteur principal de l'application. Tout le reste de la fenêtre
//! — revue, terminaux, branches — parle du worktree choisi ici.

use std::path::PathBuf;

use gpui::{div, prelude::*, px, App, Context, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    menu::{ContextMenuExt, PopupMenuItem},
    v_flex, ActiveTheme, Sizable, StyledExt, WindowExt,
};

use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::settings::Settings;

/// Ce qu'une ligne de la barre latérale affiche d'un worktree.
///
/// Le résumé vaut `None` tant que le premier balayage n'est pas revenu : une
/// ligne vide se lit « on ne sait pas encore », ce qui est vrai, là où un zéro
/// affirmerait à tort qu'il n'y a rien à relire.
struct WorktreeRow {
    path: PathBuf,
    label: String,
    branch: Option<String>,
    is_main: bool,
    prunable: bool,
    summary: Option<crate::git::Summary>,
    agent: Option<crate::ui::app::AgentState>,
}

/// Le volume de travail en cours : lignes ajoutées et retirées.
///
/// Le nombre de fichiers n'y figure que faute de mieux — un renommage ou un
/// binaire ne fait bouger aucune ligne, et ne rien montrer laisserait croire
/// qu'il n'y a rien.
fn volume(summary: crate::git::Summary, cx: &gpui::App) -> impl IntoElement {
    let colors = crate::ui::theme::DiffColors::of(cx);
    h_flex()
        .flex_none()
        .gap_1()
        .text_xs()
        .when(summary.added > 0, |el| {
            el.child(
                div()
                    .text_color(colors.added_fg)
                    .child(format!("+{}", summary.added)),
            )
        })
        .when(summary.removed > 0, |el| {
            el.child(
                div()
                    .text_color(colors.removed_fg)
                    .child(format!("−{}", summary.removed)),
            )
        })
        .when(summary.added == 0 && summary.removed == 0, |el| {
            el.child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(summary.files.to_string()),
            )
        })
}

/// La pastille d'un agent : pleine quand il travaille, creuse quand il attend.
///
/// Une pastille et non un mot : la ligne porte déjà un nom et une branche, et
/// c'est une information qu'on lit du coin de l'œil en parcourant la liste.
fn agent_badge(agent: &crate::ui::app::AgentState, cx: &gpui::App) -> impl IntoElement {
    let color = if agent.working {
        cx.theme().warning
    } else {
        cx.theme().muted_foreground
    };
    h_flex()
        .flex_none()
        .gap_1()
        .items_center()
        .child(
            div()
                .size(px(7.))
                .rounded_full()
                .when(agent.working, |el| el.bg(color))
                .when(!agent.working, |el| {
                    el.border_1().border_color(color.opacity(0.8))
                }),
        )
        // Deux agents dans le même worktree, cela arrive : on le dit plutôt
        // que de laisser croire qu'il n'y en a qu'un.
        .when(agent.count > 1, |el| {
            el.child(
                div()
                    .text_xs()
                    .text_color(color)
                    .child(agent.count.to_string()),
            )
        })
        // Le nom de l'agent dès qu'il y a plus d'un profil à distinguer : la
        // pastille dit qu'il s'en passe quelque chose, elle ne dit pas qui.
        .child(
            div()
                .text_xs()
                .text_color(color)
                .child(agent.programs.join(", ")),
        )
}

/// Le nom d'un dépôt qu'on n'a pas pu ouvrir.
///
/// Déduit du chemin et non demandé à git, qui ne peut justement pas répondre :
/// c'est le dernier segment, celui qu'on reconnaît, et le chemin entier reste
/// sur la ligne d'en dessous.
fn repo_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

impl ClaudhubApp {
    pub(super) fn render_sidebar(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let sidebar_scroll = self.scroll_of("sidebar");
        let find = self.render_find(crate::ui::find::Pane::Sidebar, cx);
        let query = self.query(crate::ui::find::Pane::Sidebar, cx);
        // Le `wt.toml` de chaque dépôt, demandé une fois : c'est lui qui
        // décide de ce que le menu d'un worktree propose.
        let mains: Vec<PathBuf> = self.repos.iter().map(|repo| repo.main.clone()).collect();
        for main in &mains {
            self.ensure_wt_project(main);
        }
        let repos: Vec<_> =
            self.repos
                .iter()
                .enumerate()
                .map(|(ix, repo)| {
                    (
                        ix,
                        repo.main.clone(),
                        repo.name.clone(),
                        repo.collapsed,
                        repo.worktrees
                            .iter()
                            .map(|w| WorktreeRow {
                                path: w.path.clone(),
                                label: w.label(),
                                branch: w.branch.clone(),
                                is_main: w.is_main,
                                prunable: w.prunable,
                                summary: self.summaries.get(&w.path).copied(),
                                agent: self.agents.get(&w.path).cloned(),
                            })
                            .filter(|row| {
                                crate::ui::find::matches(&query, &row.label)
                                    || row.branch.as_deref().is_some_and(|branch| {
                                        crate::ui::find::matches(&query, branch)
                                    })
                            })
                            .collect::<Vec<_>>(),
                    )
                })
                // Un dépôt dont le nom correspond reste entier — c'est le geste
                // « montre-moi ce dépôt ». Sinon il ne reste que s'il porte un
                // worktree trouvé : un dépôt réduit à son titre se lirait comme un
                // dépôt vide.
                .filter(|(_, _, name, _, worktrees)| {
                    crate::ui::find::matches(&query, name) || !worktrees.is_empty()
                })
                .collect();
        // Les dépôts qui ne s'ouvrent pas, filtrés comme les autres : la
        // recherche porte sur ce qu'on voit.
        let unavailable: Vec<(PathBuf, SharedString, SharedString)> = self
            .unavailable
            .iter()
            .map(|repo| {
                (
                    repo.path.clone(),
                    SharedString::from(repo_name(&repo.path)),
                    SharedString::from(repo.message.clone()),
                )
            })
            .filter(|(_, name, _)| crate::ui::find::matches(&query, name))
            .collect();
        let active = self.active.clone();
        let empty = repos.is_empty() && unavailable.is_empty();

        // Ni fond `sidebar`, ni filet droit : des reliques de l'époque des
        // panneaux cousus bord à bord. Le fond vient de la carte — un jeton
        // proche-mais-pas-égal repeindrait par-dessus ses coins arrondis, et
        // c'est ce qui faisait de « Dépôts » la seule carte d'une autre
        // couleur, carrée en bas.
        v_flex()
            .size_full()
            .min_w(px(160.))
            .child(
                h_flex()
                    .py_1()
                    .px_2()
                    .gap_1()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(cx.theme().muted_foreground)
                            .child(tr!("sidebar-repositories")),
                    )
                    .child(
                        Button::new("add-repo")
                            .ghost()
                            .xsmall()
                            .icon(icon("plus"))
                            .tooltip(tr!("sidebar-open-repository"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.prompt_open_repository(window, cx);
                            })),
                    ),
            )
            .children(find)
            .child(
                div().flex_1().min_h_0().child(
                    self.scrolled(
                        "sidebar-bar",
                        &sidebar_scroll,
                        crate::ui::motion::Axes::Vertical,
                        window,
                        div()
                            .id("sidebar-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&sidebar_scroll)
                            // Au premier lancement, c'est tout ce qu'on voit : un
                            // texte gris ne dit pas quoi faire, un bouton si.
                            .when(empty, |el| {
                                el.child(
                                    v_flex()
                                        .p_4()
                                        .gap_2()
                                        .items_center()
                                        .child(
                                            icon("folder").large().text_color(
                                                cx.theme().muted_foreground.opacity(0.4),
                                            ),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_center()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(tr!("sidebar-empty")),
                                        )
                                        .child(
                                            Button::new("open-first-repo")
                                                .outline()
                                                .small()
                                                .label(tr!("sidebar-open-repository"))
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.prompt_open_repository(window, cx);
                                                })),
                                        ),
                                )
                            })
                            .children(repos.into_iter().map(
                                |(ix, main, name, collapsed, worktrees)| {
                                    let main_for_add = main.clone();
                                    let main_for_menu = main.clone();
                                    v_flex()
                                        .child(
                                            h_flex()
                                                .id(("repo", ix))
                                                .py_1()
                                                .px_2()
                                                .gap_1()
                                                .items_center()
                                                .cursor_pointer()
                                                .hover(|s| {
                                                    s.bg(cx.theme().sidebar_accent.opacity(0.6))
                                                })
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    if let Some(repo) = this.repos.get_mut(ix) {
                                                        repo.collapsed = !repo.collapsed;
                                                    }
                                                    cx.notify();
                                                }))
                                                .child(
                                                    icon(if collapsed {
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
                                                        .font_semibold()
                                                        .child(name),
                                                )
                                                .child(
                                                    Button::new(("add-worktree", ix))
                                                        .ghost()
                                                        .xsmall()
                                                        .icon(icon("plus"))
                                                        .tooltip(tr!("sidebar-new-worktree"))
                                                        .on_click(cx.listener(
                                                            move |this, _, window, cx| {
                                                                this.prompt_new_worktree(
                                                                    main_for_add.clone(),
                                                                    window,
                                                                    cx,
                                                                );
                                                            },
                                                        )),
                                                )
                                                // Retirer un dépôt de la liste
                                                // ne touche à rien sur le
                                                // disque, mais cela ferme tout
                                                // ce qu'on y avait ouvert : au
                                                // clic droit, comme les gestes
                                                // qu'on ne fait pas deux fois
                                                // par jour.
                                                .context_menu({
                                                    let entity = cx.entity();
                                                    let main = main_for_menu.clone();
                                                    move |menu, _window, _cx| {
                                                        let (entity, main) =
                                                            (entity.clone(), main.clone());
                                                        menu.item(
                                                            PopupMenuItem::new(tr!(
                                                                "sidebar-forget-repository"
                                                            ))
                                                            .icon(icon("x"))
                                                            .on_click(move |_, window, cx| {
                                                                entity.update(cx, |this, cx| {
                                                                    this.forget_repository(
                                                                        main.clone(),
                                                                        window,
                                                                        cx,
                                                                    );
                                                                });
                                                            }),
                                                        )
                                                    }
                                                }),
                                        )
                                        .when(!collapsed, |el| {
                                            el.children(worktrees.into_iter().enumerate().map(
                                                |(wix, worktree)| {
                                                    let WorktreeRow {
                                                        path,
                                                        label,
                                                        branch,
                                                        is_main,
                                                        prunable,
                                                        summary,
                                                        agent,
                                                    } = worktree;
                                                    let selected =
                                                        active.as_deref() == Some(path.as_path());
                                                    let for_click = path.clone();
                                                    let for_remove = path.clone();
                                                    let for_menu = path.clone();
                                                    let repo_main = main.clone();
                                                    let for_menu_main = main.clone();
                                                    h_flex()
                                                        .id(("worktree", ix * 1000 + wix))
                                                        // Pas de hauteur fixe : la
                                                        // ligne porte deux lignes de
                                                        // texte, et une hauteur figée
                                                        // les faisait déborder sur la
                                                        // ligne suivante dès qu'on
                                                        // grossissait la police.
                                                        .py_1()
                                                        .pl_5()
                                                        .pr_1()
                                                        .mx_1()
                                                        .rounded(cx.theme().radius)
                                                        .gap_1()
                                                        .items_center()
                                                        .cursor_pointer()
                                                        .border_l_2()
                                                        .border_color(gpui::transparent_black())
                                                        .when(selected, |el| {
                                                            el.bg(cx.theme().sidebar_accent)
                                                                .border_color(cx.theme().primary)
                                                                .text_color(
                                                                    cx.theme()
                                                                        .sidebar_accent_foreground,
                                                                )
                                                        })
                                                        .hover(|s| {
                                                            s.bg(cx
                                                                .theme()
                                                                .sidebar_accent
                                                                .opacity(0.5))
                                                        })
                                                        .on_click(cx.listener(
                                                            move |this, _, window, cx| {
                                                                this.select_worktree(
                                                                    for_click.clone(),
                                                                    window,
                                                                    cx,
                                                                );
                                                            },
                                                        ))
                                                        .child(
                                                            icon(if is_main {
                                                                "folder"
                                                            } else {
                                                                "git-branch"
                                                            })
                                                            .xsmall(),
                                                        )
                                                        .child(
                                                            v_flex()
                                                                .flex_1()
                                                                .min_w_0()
                                                                .child(
                                                                    div()
                                                                        .truncate()
                                                                        .text_sm()
                                                                        .when(selected, |el| {
                                                                            el.font_semibold()
                                                                        })
                                                                        .child(label),
                                                                )
                                                                .when_some(branch, |el, branch| {
                                                                    el.child(
                                                                    div()
                                                                        .truncate()
                                                                        .text_xs()
                                                                        .text_color(
                                                                            cx.theme()
                                                                                .muted_foreground,
                                                                        )
                                                                        .child(branch),
                                                                )
                                                                }),
                                                        )
                                                        // Ce que ce worktree a en
                                                        // chantier, et qui y
                                                        // travaille : les deux
                                                        // questions qu'on se pose
                                                        // en parcourant la liste.
                                                        .when_some(agent, |el, agent| {
                                                            el.child(agent_badge(&agent, cx))
                                                        })
                                                        .when_some(
                                                            summary.filter(|s| !s.is_empty()),
                                                            |el, summary| {
                                                                el.child(volume(summary, cx))
                                                            },
                                                        )
                                                        // Ce que `wt` sait de lui :
                                                        // démarré ou non, et
                                                        // l'adresse qu'il expose.
                                                        .children(self.render_wt_state(&path, cx))
                                                        .children(self.render_wt_links(&path, cx))
                                                        .when(prunable, |el| {
                                                            el.child(
                                                                icon("alert-circle")
                                                                    .xsmall()
                                                                    .text_color(cx.theme().warning),
                                                            )
                                                        })
                                                        // Le worktree principal ne se
                                                        // retire pas : git refuse, et
                                                        // proposer le bouton reviendrait
                                                        // à promettre une erreur.
                                                        .when(!is_main, |el| {
                                                            el.child(
                                                            Button::new((
                                                                "rm-worktree",
                                                                ix * 1000 + wix,
                                                            ))
                                                            .ghost()
                                                            .xsmall()
                                                            .icon(icon("trash-2"))
                                                            .tooltip(tr!("sidebar-remove-worktree"))
                                                            .on_click(cx.listener(
                                                                move |this, _, window, cx| {
                                                                    this.confirm_remove_worktree(
                                                                        repo_main.clone(),
                                                                        for_remove.clone(),
                                                                        window,
                                                                        cx,
                                                                    );
                                                                },
                                                            )),
                                                        )
                                                        })
                                                        // Le clic droit porte tout
                                                        // ce que le projet ajoute :
                                                        // Claudhub ne connaît ni
                                                        // ses tâches ni ses hooks,
                                                        // il les affiche.
                                                        .context_menu({
                                                            let entity = cx.entity();
                                                            let (main, path) = (
                                                                for_menu_main.clone(),
                                                                for_menu.clone(),
                                                            );
                                                            move |menu, _window, cx| {
                                                                let (main, path) =
                                                                    (main.clone(), path.clone());
                                                                entity.update(cx, |this, cx| {
                                                                    this.worktree_menu(
                                                                        menu, main, path, cx,
                                                                    )
                                                                })
                                                            }
                                                        })
                                                },
                                            ))
                                        })
                                },
                            ))
                            // Les dépôts introuvables en dernier : ce sont des
                            // reliques, pas des dépôts de travail, et les
                            // laisser à leur rang d'origine ferait un trou au
                            // milieu de la liste.
                            .children(unavailable.into_iter().enumerate().map(
                                |(ix, (path, name, message))| {
                                    let for_forget = path.clone();
                                    h_flex()
                                        .id(("unavailable", ix))
                                        .py_1()
                                        .px_2()
                                        .gap_1()
                                        .items_center()
                                        .tooltip(move |window, cx| {
                                            gpui_component::tooltip::Tooltip::new(message.clone())
                                                .build(window, cx)
                                        })
                                        .child(
                                            icon("triangle-alert")
                                                .xsmall()
                                                .text_color(cx.theme().warning),
                                        )
                                        .child(
                                            v_flex()
                                                .flex_1()
                                                .min_w_0()
                                                .child(
                                                    div()
                                                        .truncate()
                                                        .text_sm()
                                                        .text_color(cx.theme().muted_foreground)
                                                        .child(name),
                                                )
                                                .child(
                                                    div()
                                                        .truncate()
                                                        .text_xs()
                                                        .text_color(cx.theme().warning)
                                                        .child(tr!("sidebar-repo-unavailable")),
                                                ),
                                        )
                                        // Un bouton et non une entrée de menu :
                                        // c'est la seule chose qu'on puisse
                                        // faire d'une ligne pareille, et la
                                        // cacher derrière un clic droit
                                        // reviendrait à ne pas la proposer.
                                        .child(
                                            Button::new(("forget-repo", ix))
                                                .ghost()
                                                .xsmall()
                                                .icon(icon("x"))
                                                .tooltip(tr!("sidebar-forget-repository"))
                                                .on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        this.forget_repository(
                                                            for_forget.clone(),
                                                            window,
                                                            cx,
                                                        );
                                                    },
                                                )),
                                        )
                                },
                            )),
                        cx,
                    ),
                ),
            )
    }

    /// Retire un dépôt de la liste. Rien n'est touché sur le disque.
    ///
    /// Le même geste pour un dépôt ouvert et pour un dépôt introuvable : dans
    /// les deux cas, ce qu'on retire est une entrée de la liste des dépôts
    /// rouverts au démarrage, et le second cas est le seul où l'on ne pouvait
    /// pas le faire — d'où le signalement.
    pub(super) fn forget_repository(
        &mut self,
        main: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::ui::settings::Settings::update_global(cx, |s| s.forget_repository(&main));
        self.unavailable.retain(|repo| repo.path != main);
        let closed: Vec<PathBuf> = self
            .repos
            .iter()
            .filter(|repo| repo.main == main)
            .flat_map(|repo| repo.worktrees.iter().map(|w| w.path.clone()))
            .collect();
        self.repos.retain(|repo| repo.main != main);
        // Ce qu'on gardait de ses worktrees n'a plus d'objet. Le magasin
        // d'état, lui, n'est pas purgé : notes et replis attendent le jour où
        // le dépôt est rouvert, et les effacer ici ferait d'un rangement une
        // perte.
        for worktree in &closed {
            self.review.remove(worktree);
            self.terminals.remove(worktree);
            self.summaries.remove(worktree);
        }
        if self
            .active
            .as_deref()
            .is_some_and(|active| closed.iter().any(|path| path == active))
        {
            self.active = None;
            if let Some(first) = self.first_worktree() {
                self.select_worktree(first, window, cx);
            }
        }
        cx.notify();
    }

    /// Ouvre un dépôt. Le sélecteur de dossier natif est asynchrone : la
    /// réponse revient dans une tâche, d'où le `spawn`.
    fn prompt_open_repository(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return; // annulé
            };
            let _ = this.update(cx, |this, cx| {
                for path in paths {
                    match this.repo_path_for_server(path, cx) {
                        Ok(path) => this.git.send(Cmd::OpenRepo(path)),
                        Err(message) => this.announce(message, cx),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Le chemin que le sélecteur natif a rendu, tel que le serveur le
    /// comprendra.
    ///
    /// Sous Windows, le dialogue rend `\\wsl.localhost\Ubuntu\home\…` ou
    /// `C:\…` ; le fil, lui, ne transporte que des chemins Linux — c'est le
    /// disque du serveur qui fait foi. C'est l'un des rares points où un
    /// chemin **entre** par le monde Windows, et donc l'un des rares où il
    /// faut le traduire. Ailleurs, il n'y a rien à faire.
    fn repo_path_for_server(&self, path: PathBuf, cx: &App) -> Result<PathBuf, SharedString> {
        if !cfg!(windows) {
            return Ok(path);
        }
        let distro = Settings::global(cx).wsl_distro.clone();
        let Some(translated) = crate::wslpath::to_linux(&path) else {
            return Err(tr!("repo-not-in-wsl"));
        };
        // Un dépôt d'une **autre** distribution que celle du serveur : son
        // chemin est valide là-bas et introuvable ici, et l'ouvrir donnerait
        // un dossier vide sans dire pourquoi.
        if let Some(other) = translated
            .distro
            .filter(|d| !d.eq_ignore_ascii_case(&distro))
        {
            return Err(tr!("repo-other-distro", { distro: other }));
        }
        Ok(translated.path)
    }

    fn prompt_new_worktree(&mut self, main: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.open_text_dialog(
            tr!("worktree-new-title"),
            tr!("worktree-new-placeholder"),
            window,
            cx,
            // Le nom saisi, puis ce que le projet demande : ses `[[prompt]]`
            // deviennent un dialogue, ses copies et ses ports sont faits par
            // `wt`. Sans `wt.toml`, l'ajout git nu suffit — un dépôt sans
            // configuration doit pouvoir gagner un worktree quand même.
            move |this, name, window, cx| {
                this.start_worktree(main.clone(), name, None, window, cx);
            },
        );
    }

    fn confirm_remove_worktree(
        &mut self,
        main: PathBuf,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let label = path.display().to_string();
        let entity = cx.entity();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (main, path, entity) = (main.clone(), path.clone(), entity.clone());
            dialog
                .title(tr!("worktree-remove-title"))
                .child(div().text_sm().child(label.clone()))
                .overlay_closable(false)
                .close_button(false)
                .on_ok(move |_, _window, cx| {
                    entity.update(cx, |this, cx| {
                        this.git.send(Cmd::RemoveWorktree {
                            main: main.clone(),
                            path: path.clone(),
                            // Sans `force`, git refuse de retirer un worktree
                            // qui a des modifications — c'est la protection
                            // qu'on veut ici, et le message le dira.
                            force: false,
                        });
                        cx.notify();
                    });
                    true
                })
        });
    }
}
