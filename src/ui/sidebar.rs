//! Sidebar: the repositories and their worktrees.
//!
//! It is the application's main picker. Everything else in the window — review,
//! terminals, branches — talks about the worktree chosen here.

use std::path::{Path, PathBuf};

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

/// The volume of work in progress: lines added and removed.
///
/// The file count is only there for want of better — a rename or a binary moves
/// no line, and showing nothing would suggest there is nothing.
fn volume(summary: crate::git::Summary, cx: &gpui::App) -> impl IntoElement {
    let colors = crate::ui::theme::DiffColors::of(cx);
    h_flex()
        .flex_none()
        .gap_1()
        .text_xs()
        .children(crate::ui::theme::volume(
            summary.added,
            summary.removed,
            &colors,
        ))
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

/// The name of a repository we could not open.
///
/// Derived from the path and not asked of git, which is precisely what cannot
/// answer: it is the last segment, the one that is recognised, and the whole
/// path stays on the line below.
fn repo_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// A repository as the sidebar shows it, with the worktrees the search kept.
struct RepoRow {
    ix: usize,
    main: PathBuf,
    name: String,
    collapsed: bool,
    worktrees: Vec<WorktreeRow>,
}

/// A repository that no longer opens.
struct UnavailableRow {
    path: PathBuf,
    name: SharedString,
    message: SharedString,
}

/// A repository that could not be opened: what it was called, and a button to
/// forget it.
///
/// A button and not a menu entry: it is the only thing that can be done with
/// such a row, and hiding it behind a right click would amount to not offering
/// it.
fn render_unavailable(
    ix: usize,
    repo: UnavailableRow,
    cx: &mut Context<ClaudhubApp>,
) -> impl IntoElement {
    let UnavailableRow {
        path,
        name,
        message,
    } = repo;
    h_flex()
        .id(("unavailable", ix))
        .py_1()
        .px_2()
        .gap_1()
        .items_center()
        .tooltip(move |window, cx| {
            gpui_component::tooltip::Tooltip::new(message.clone()).build(window, cx)
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
        .child(
            Button::new(("forget-repo", ix))
                .ghost()
                .xsmall()
                .icon(icon("x"))
                .tooltip(tr!("sidebar-forget-repository"))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.forget_repository(path.clone(), window, cx);
                })),
        )
}

impl ClaudhubApp {
    pub(super) fn render_sidebar(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let sidebar_scroll = self.scroll_of("sidebar");
        let find = self.render_find(crate::ui::find::Pane::Sidebar, cx);
        let (repos, unavailable) = self.sidebar_rows(cx);
        let active = self.active.clone();
        let empty = repos.is_empty() && unavailable.is_empty();

        // The rows are built before the scrolling frame rather than inside it:
        // `scrolled` takes the application, and its content reads it.
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        for repo in repos {
            rows.push(
                self.render_repo(repo, active.as_deref(), cx)
                    .into_any_element(),
            );
        }
        // The unreachable repositories last: they are relics, not working
        // repositories, and leaving them in their original rank would make a
        // hole in the middle of the list.
        for (ix, repo) in unavailable.into_iter().enumerate() {
            rows.push(render_unavailable(ix, repo, cx).into_any_element());
        }

        // Neither a `sidebar` background nor a right-hand rule: relics of the
        // days of panels stitched edge to edge. The background comes from the
        // card — a close-but-not-equal token would paint over its rounded
        // corners, and that is what made "Repositories" the one card of another
        // colour, square at the bottom.
        v_flex()
            .size_full()
            .min_w(px(160.))
            .child(self.render_sidebar_header(cx))
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
                            // On first launch this is all one sees: grey text
                            // does not say what to do, a button does.
                            .when(empty, |el| el.child(self.render_sidebar_empty(cx)))
                            .children(rows),
                        cx,
                    ),
                ),
            )
    }

    /// What the sidebar shows, filtered by the search, in the order it shows it.
    fn sidebar_rows(&mut self, cx: &mut Context<Self>) -> (Vec<RepoRow>, Vec<UnavailableRow>) {
        let query = self.query(crate::ui::find::Pane::Sidebar, cx);
        // Each repository's `wt.toml`, asked for once: it is what decides what a
        // worktree's menu offers.
        let mains: Vec<PathBuf> = self.repos.iter().map(|repo| repo.main.clone()).collect();
        for main in &mains {
            self.ensure_wt_project(main);
        }
        let repos: Vec<RepoRow> = self
            .repos
            .iter()
            .enumerate()
            .map(|(ix, repo)| RepoRow {
                ix,
                main: repo.main.clone(),
                name: repo.name.clone(),
                collapsed: repo.collapsed,
                worktrees: repo
                    .worktrees
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
                            || row
                                .branch
                                .as_deref()
                                .is_some_and(|branch| crate::ui::find::matches(&query, branch))
                    })
                    .collect(),
            })
            // A repository whose name matches stays whole — that is the "show me
            // this repository" gesture. Otherwise it only stays if it carries a
            // matching worktree: a repository reduced to its title would read as
            // an empty repository.
            .filter(|repo| {
                crate::ui::find::matches(&query, &repo.name) || !repo.worktrees.is_empty()
            })
            .collect();
        // The repositories that do not open, filtered like the rest: the search
        // applies to what is on screen.
        let unavailable: Vec<UnavailableRow> = self
            .unavailable
            .iter()
            .map(|repo| UnavailableRow {
                path: repo.path.clone(),
                name: SharedString::from(repo_name(&repo.path)),
                message: SharedString::from(repo.message.clone()),
            })
            .filter(|repo| crate::ui::find::matches(&query, &repo.name))
            .collect();
        (repos, unavailable)
    }

    fn render_sidebar_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
            )
    }

    fn render_sidebar_empty(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .p_4()
            .gap_2()
            .items_center()
            .child(
                icon("folder")
                    .large()
                    .text_color(cx.theme().muted_foreground.opacity(0.4)),
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
            )
    }

    /// A repository and, unless it is collapsed, its worktrees.
    fn render_repo(
        &self,
        repo: RepoRow,
        active: Option<&Path>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let RepoRow {
            ix,
            main,
            name,
            collapsed,
            worktrees,
        } = repo;
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        if !collapsed {
            for (wix, worktree) in worktrees.into_iter().enumerate() {
                let selected = active == Some(worktree.path.as_path());
                rows.push(
                    self.render_worktree(ix * 1000 + wix, &main, worktree, selected, cx)
                        .into_any_element(),
                );
            }
        }
        v_flex()
            .child(self.render_repo_header(ix, &main, name, collapsed, cx))
            .children(rows)
    }

    fn render_repo_header(
        &self,
        ix: usize,
        main: &Path,
        name: String,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let for_add = main.to_path_buf();
        let for_menu = main.to_path_buf();
        h_flex()
            .id(("repo", ix))
            .py_1()
            .px_2()
            .gap_1()
            .items_center()
            .cursor_pointer()
            .hover(|s| s.bg(cx.theme().sidebar_accent.opacity(0.6)))
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
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.prompt_new_worktree(for_add.clone(), window, cx);
                    })),
            )
            // Removing a repository from the list touches nothing on disk, but
            // it does close everything that was open in it: on right click, like
            // the gestures one does not make twice a day.
            .context_menu({
                let entity = cx.entity();
                move |menu, _window, _cx| {
                    let (entity, main) = (entity.clone(), for_menu.clone());
                    menu.item(
                        PopupMenuItem::new(tr!("sidebar-forget-repository"))
                            .icon(icon("x"))
                            .on_click(move |_, window, cx| {
                                entity.update(cx, |this, cx| {
                                    this.forget_repository(main.clone(), window, cx);
                                });
                            }),
                    )
                }
            })
    }

    fn render_worktree(
        &self,
        key: usize,
        main: &Path,
        worktree: WorktreeRow,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let WorktreeRow {
            path,
            label,
            branch,
            is_main,
            prunable,
            summary,
            agent,
        } = worktree;
        let for_click = path.clone();
        let for_remove = path.clone();
        let for_menu = path.clone();
        let repo_main = main.to_path_buf();
        let for_menu_main = main.to_path_buf();
        h_flex()
            .id(("worktree", key))
            // No fixed height: the row carries two lines of text, and a frozen
            // height made them spill onto the next row as soon as the font grew.
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
                    .text_color(cx.theme().sidebar_accent_foreground)
            })
            .hover(|s| s.bg(cx.theme().sidebar_accent.opacity(0.5)))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_worktree(for_click.clone(), window, cx);
            }))
            .child(icon(if is_main { "folder" } else { "git-branch" }).xsmall())
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .when(selected, |el| el.font_semibold())
                            .child(label),
                    )
                    .when_some(branch, |el, branch| {
                        el.child(
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(branch),
                        )
                    }),
            )
            // What this worktree has in progress, and who is working on it: the
            // two questions one asks while scanning the list.
            .when_some(agent, |el, agent| el.child(agent_badge(&agent, cx)))
            .when_some(summary.filter(|s| !s.is_empty()), |el, summary| {
                el.child(volume(summary, cx))
            })
            // What `wt` knows about it: started or not, and the address it
            // exposes.
            .children(self.render_wt_state(&path, cx))
            .children(self.render_wt_links(&path, cx))
            .when(prunable, |el| {
                el.child(icon("alert-circle").xsmall().text_color(cx.theme().warning))
            })
            // The main worktree is not removable: git refuses, and offering the
            // button would promise an error.
            .when(!is_main, |el| {
                el.child(
                    Button::new(("rm-worktree", key))
                        .ghost()
                        .xsmall()
                        .icon(icon("trash-2"))
                        .tooltip(tr!("sidebar-remove-worktree"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.confirm_remove_worktree(
                                repo_main.clone(),
                                for_remove.clone(),
                                window,
                                cx,
                            );
                        })),
                )
            })
            // The right click carries everything the project adds: Claudhub
            // knows neither its tasks nor its hooks, it shows them.
            .context_menu({
                let entity = cx.entity();
                move |menu, _window, cx| {
                    let (main, path) = (for_menu_main.clone(), for_menu.clone());
                    entity.update(cx, |this, cx| this.worktree_menu(menu, main, path, cx))
                }
            })
    }
    /// Removes a repository from the list. Nothing on disk is touched.
    ///
    /// The same gesture for an open repository and for an unreachable one: in
    /// both cases what is removed is an entry from the list of repositories
    /// reopened at startup, and the second case is the only one where it could
    /// not be done — hence the report.
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
                return; // cancelled
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
