//! Créer, démarrer, intégrer et retirer un worktree.
//!
//! Deux sources se rejoignent ici : git, qui sait ajouter un checkout et
//! fusionner une branche, et le `wt.toml` du projet, qui sait ce qu'il faut
//! copier, quels ports allouer et quoi lancer ensuite.
//!
//! **Le `wt.toml` est le système d'extension de Claudhub.** Les `[tasks.*]`
//! d'un projet apparaissent dans le menu d'un worktree sans que Claudhub sache
//! ce qu'elles font, ses `[[prompt]]` deviennent un dialogue, son
//! `[status] up` une pastille dans la barre latérale. Rien de tout cela n'est
//! compilé ici : c'est le fichier du projet qui le déclare.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gpui::{div, prelude::*, px, Context, Entity, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputState},
    menu::{DropdownMenu, PopupMenuItem},
    v_flex, ActiveTheme, Sizable, WindowExt,
};

use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::settings::Settings;
use crate::wt;

/// La création guidée d'un worktree, entre le nom saisi et la commande finale.
///
/// Elle vit sur l'application et non dans le dialogue : les questions du
/// projet arrivent par un événement — un `[[prompt]]` avec `source` lance un
/// shell — et une fermeture de dialogue n'a nulle part où les ranger.
pub struct Creation {
    pub main: PathBuf,
    pub slug: String,
    /// Point de départ de la branche, quand on part d'une branche existante.
    pub from: Option<String>,
    pub questions: Vec<wt::Question>,
    pub answers: BTreeMap<String, String>,
    /// Les champs libres, créés à l'arrivée des questions et jamais dans un
    /// rendu : recréés par frame, ils perdraient le curseur à la première
    /// frappe.
    pub inputs: BTreeMap<String, Entity<InputState>>,
    /// Une demande de questions est partie et n'est pas revenue.
    pub asking: bool,
}

impl ClaudhubApp {
    // — Ce que le projet déclare ————————————————————————————————

    /// Le `wt.toml` d'un dépôt, s'il en a un et qu'on l'a déjà lu.
    pub(super) fn wt_project(&self, main: &Path) -> Option<&wt::Snapshot> {
        self.wt_projects.get(main)?.as_ref()
    }

    /// Demande le `wt.toml` d'un dépôt, une fois.
    pub(super) fn ensure_wt_project(&mut self, main: &Path) {
        if self.wt_projects.contains_key(main) {
            return;
        }
        // Marqué tout de suite comme lu : sans cela, chaque frame du menu
        // relancerait la commande pendant tout le temps de la lecture.
        self.wt_projects.insert(main.to_path_buf(), None);
        self.git.send(Cmd::WtLoad {
            main: main.to_path_buf(),
        });
    }

    pub(super) fn wt_state(
        &self,
        worktree: &Path,
    ) -> Option<&crate::runtime::protocol::WtWorktree> {
        self.wt_states.get(worktree)
    }

    /// Le relevé de fond : état et adresses de chaque worktree que `wt` gère.
    pub(super) fn scan_wt(&mut self) {
        let targets: Vec<(PathBuf, PathBuf)> = self
            .repos
            .iter()
            .filter(|repo| self.wt_project(&repo.main).is_some())
            .flat_map(|repo| {
                repo.worktrees
                    .iter()
                    .map(|w| (repo.main.clone(), w.path.clone()))
            })
            .collect();
        if targets.is_empty() {
            return;
        }
        self.git.send(Cmd::WtScan { targets });
    }

    // — Créer ————————————————————————————————————————————————

    /// Démarre la création guidée d'un worktree.
    ///
    /// Sans `wt.toml`, on retombe sur l'ajout git nu : un dépôt sans
    /// configuration doit pouvoir gagner un worktree quand même.
    pub(super) fn start_worktree(
        &mut self,
        main: PathBuf,
        slug: String,
        from: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let slug = slug.trim().to_string();
        if slug.is_empty() {
            return;
        }
        let Some(project) = self.wt_project(&main).cloned() else {
            self.add_worktree_without_wt(&main, &slug, from.as_deref(), cx);
            return;
        };
        if !project.has_prompts {
            self.git.send(Cmd::WtCreate {
                main,
                slug,
                from,
                answers: BTreeMap::new(),
            });
            cx.notify();
            return;
        }
        self.creation = Some(Creation {
            main: main.clone(),
            slug: slug.clone(),
            from,
            questions: Vec::new(),
            answers: BTreeMap::new(),
            inputs: BTreeMap::new(),
            asking: true,
        });
        self.git.send(Cmd::WtQuestions {
            main,
            slug,
            answers: BTreeMap::new(),
        });
        // Le dialogue s'ouvre tout de suite, avec sa mention d'attente : les
        // questions passent par un shell, et une fenêtre qui ne réagit pas
        // pendant une seconde se lit comme un clic perdu.
        self.open_creation_dialog(window, cx);
        cx.notify();
    }

    /// L'ajout git nu, pour un dépôt sans `wt.toml`.
    ///
    /// Le worktree est créé à côté du dépôt, dans `<dépôt>-wt/<nom>` : c'est la
    /// convention de `wt`, pour que les deux outils voient les mêmes dossiers
    /// le jour où le projet se dote d'une configuration.
    fn add_worktree_without_wt(
        &mut self,
        main: &Path,
        slug: &str,
        from: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let repo_name = main
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repo".into());
        let root = main
            .parent()
            .map(|p| p.join(format!("{repo_name}-wt")))
            .unwrap_or_else(|| PathBuf::from(format!("{repo_name}-wt")));
        self.git.send(Cmd::AddWorktree {
            main: main.to_path_buf(),
            path: root.join(slug),
            branch: format!("wt/{slug}"),
            from: from.map(str::to_string),
        });
        cx.notify();
    }

    /// Reçoit les questions du projet et prépare leurs champs.
    pub(super) fn wt_questions_arrived(
        &mut self,
        main: PathBuf,
        slug: String,
        answers: BTreeMap<String, String>,
        questions: Vec<wt::Question>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(creation) = self.creation.as_mut() else {
            return;
        };
        // Une réponse en retard, pour un autre worktree ou un tour plus
        // ancien, remplacerait les questions par les mauvaises.
        if creation.main != main || creation.slug != slug || creation.answers != answers {
            return;
        }
        creation.asking = false;
        // Plus rien à demander : le projet a fini de poser ses questions.
        if questions.is_empty() {
            let (main, slug, from, answers) = (
                creation.main.clone(),
                creation.slug.clone(),
                creation.from.clone(),
                creation.answers.clone(),
            );
            self.creation = None;
            window.close_all_dialogs(cx);
            self.git.send(Cmd::WtCreate {
                main,
                slug,
                from,
                answers,
            });
            cx.notify();
            return;
        }
        // Les valeurs par défaut sont posées tout de suite : une question à
        // choix unique dont on ne touche pas au menu doit partir avec sa
        // valeur proposée, pas avec rien.
        let mut inputs = BTreeMap::new();
        for question in &questions {
            let value = question.default.clone().unwrap_or_else(|| {
                match question.kind {
                    // Un choix qui n'a pas de défaut prend le premier : c'est
                    // ce que le menu montre, et le laisser vide ferait mentir
                    // l'affichage.
                    wt::Kind::Choice => question
                        .choices
                        .first()
                        .map(|c| c.value.clone())
                        .unwrap_or_default(),
                    wt::Kind::Confirm => "0".into(),
                    _ => String::new(),
                }
            });
            if matches!(question.kind, wt::Kind::Text) {
                let start = value.clone();
                let input = cx.new(|cx| InputState::new(window, cx).default_value(start));
                inputs.insert(question.name.clone(), input);
            }
            creation.answers.insert(question.name.clone(), value);
        }
        creation.questions = questions;
        creation.inputs = inputs;
        cx.notify();
    }

    /// Note une réponse et redemande les questions : un `when` peut en
    /// débloquer une autre.
    fn answer(&mut self, name: String, value: String, cx: &mut Context<Self>) {
        let Some(creation) = self.creation.as_mut() else {
            return;
        };
        creation.answers.insert(name, value);
        cx.notify();
    }

    /// Valide la page courante et demande la suite.
    fn submit_answers(&mut self, cx: &mut Context<Self>) {
        let Some(creation) = self.creation.as_mut() else {
            return;
        };
        // Les champs libres ne se lisent qu'ici : les écouter à la frappe
        // relancerait un shell par caractère, chaque question ayant un `when`
        // qui peut en dépendre.
        let typed: Vec<(String, String)> = creation
            .inputs
            .iter()
            .map(|(name, input)| (name.clone(), input.read(cx).value().to_string()))
            .collect();
        for (name, value) in typed {
            creation.answers.insert(name, value);
        }
        creation.asking = true;
        let (main, slug, answers) = (
            creation.main.clone(),
            creation.slug.clone(),
            creation.answers.clone(),
        );
        self.git.send(Cmd::WtQuestions {
            main,
            slug,
            answers,
        });
        cx.notify();
    }

    /// Le dialogue de création.
    ///
    /// Il se redessine à chaque frame à partir de `Creation` : les questions
    /// arrivent après son ouverture, et un contenu figé à la construction
    /// resterait vide.
    fn open_creation_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entity = cx.entity();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let entity = entity.clone();
            let cancel = entity.clone();
            dialog
                .title(tr!("worktree-new-title"))
                .child(
                    div()
                        .w(px(520.))
                        .child(entity.clone().update(_cx, |this, cx| {
                            this.render_creation_body(cx).into_any_element()
                        })),
                )
                .confirm()
                .on_ok(move |_, _window, cx| {
                    entity.update(cx, |this, cx| this.submit_answers(cx));
                    // Le dialogue reste ouvert : la page suivante s'y affiche,
                    // et c'est l'absence de nouvelle question qui le ferme.
                    false
                })
                .on_cancel(move |_, _window, cx| {
                    cancel.update(cx, |this, _| this.creation = None);
                    true
                })
        });
    }

    fn render_creation_body(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(creation) = self.creation.as_ref() else {
            return div().into_any_element();
        };
        if creation.asking {
            return div()
                .p_4()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(tr!("worktree-asking"))
                .into_any_element();
        }
        let questions = creation.questions.clone();
        let answers = creation.answers.clone();
        let inputs = creation.inputs.clone();
        let muted = cx.theme().muted_foreground;

        let mut rows = Vec::new();
        for question in questions {
            let current = answers.get(&question.name).cloned().unwrap_or_default();
            let field = match question.kind {
                wt::Kind::Text => inputs
                    .get(&question.name)
                    .map(|input| Input::new(input).small().into_any_element())
                    .unwrap_or_else(|| div().into_any_element()),
                wt::Kind::Confirm => {
                    Checkbox::new(SharedString::from(format!("wt-confirm-{}", question.name)))
                        .checked(current == "1")
                        .on_click({
                            let (entity, name, was) =
                                (cx.entity(), question.name.clone(), current == "1");
                            move |_, _window, cx| {
                                let value = if was { "0" } else { "1" };
                                entity.update(cx, |this, cx| {
                                    this.answer(name.clone(), value.into(), cx)
                                });
                            }
                        })
                        .into_any_element()
                }
                wt::Kind::Choice => self
                    .render_choice(&question, &current, cx)
                    .into_any_element(),
                wt::Kind::Multi => self
                    .render_multi(&question, &current, cx)
                    .into_any_element(),
            };
            rows.push(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().child(question.title.clone()))
                    .child(field)
                    .into_any_element(),
            );
        }

        v_flex()
            .gap_3()
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(SharedString::from(creation.slug.clone())),
            )
            .children(rows)
            .into_any_element()
    }

    fn render_choice(
        &self,
        question: &wt::Question,
        current: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let label = question
            .choices
            .iter()
            .find(|choice| choice.value == current)
            .map(|choice| choice.label.clone())
            .unwrap_or_else(|| current.to_string());
        let entity = cx.entity();
        let (name, choices) = (question.name.clone(), question.choices.clone());
        Button::new(SharedString::from(format!("wt-choice-{}", question.name)))
            .outline()
            .small()
            .label(SharedString::from(label))
            .dropdown_menu(move |menu, _window, _cx| {
                choices.iter().fold(menu, |menu, choice| {
                    let (entity, name, value) =
                        (entity.clone(), name.clone(), choice.value.clone());
                    menu.item(
                        PopupMenuItem::new(SharedString::from(choice.label.clone())).on_click(
                            move |_, _window, cx| {
                                entity.update(cx, |this, cx| {
                                    this.answer(name.clone(), value.clone(), cx)
                                });
                            },
                        ),
                    )
                })
            })
    }

    /// Un choix multiple : une case par valeur, jointes par le séparateur que
    /// le projet déclare.
    fn render_multi(
        &self,
        question: &wt::Question,
        current: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let separator = question.separator.clone();
        let chosen: Vec<String> = current
            .split(separator.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect();
        let mut boxes = Vec::new();
        for (index, choice) in question.choices.iter().enumerate() {
            let checked = chosen.iter().any(|value| value == &choice.value);
            let entity = cx.entity();
            let (name, value, separator) = (
                question.name.clone(),
                choice.value.clone(),
                separator.clone(),
            );
            let chosen = chosen.clone();
            boxes.push(
                Checkbox::new(SharedString::from(format!(
                    "wt-multi-{}-{index}",
                    question.name
                )))
                .label(SharedString::from(choice.label.clone()))
                .checked(checked)
                .on_click(move |_, _window, cx| {
                    let mut next = chosen.clone();
                    if checked {
                        next.retain(|v| v != &value);
                    } else {
                        next.push(value.clone());
                    }
                    let joined = next.join(&separator);
                    entity.update(cx, |this, cx| this.answer(name.clone(), joined, cx));
                })
                .into_any_element(),
            );
        }
        v_flex().gap_1().children(boxes)
    }

    // — Démarrer, retirer, exécuter ————————————————————————————

    pub(super) fn wt_up(&mut self, main: PathBuf, worktree: &Path, cx: &mut Context<Self>) {
        let Some(slug) = self.wt_slug(&main, worktree) else {
            return;
        };
        self.git.send(Cmd::WtUp { main, slug });
        cx.notify();
    }

    pub(super) fn wt_down(&mut self, main: PathBuf, worktree: &Path, cx: &mut Context<Self>) {
        let Some(slug) = self.wt_slug(&main, worktree) else {
            return;
        };
        self.git.send(Cmd::WtDown { main, slug });
        cx.notify();
    }

    pub(super) fn wt_remove(&mut self, main: PathBuf, worktree: &Path, cx: &mut Context<Self>) {
        let Some(slug) = self.wt_slug(&main, worktree) else {
            return;
        };
        self.git.send(Cmd::WtRemove { main, slug });
        cx.notify();
    }

    /// Lance une tâche du projet dans un onglet de terminal.
    pub(super) fn wt_task(
        &mut self,
        main: PathBuf,
        worktree: PathBuf,
        task: String,
        cx: &mut Context<Self>,
    ) {
        let Some(slug) = self.wt_slug(&main, &worktree) else {
            return;
        };
        self.git.send(Cmd::WtTask {
            main,
            worktree,
            slug,
            task,
        });
        cx.notify();
    }

    /// Reçoit une tâche prête et l'ouvre dans un terminal.
    pub(super) fn wt_task_ready(
        &mut self,
        worktree: PathBuf,
        task: String,
        launch: wt::Launch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let line = launch.shell_line();
        if line.trim().is_empty() {
            return;
        }
        let group = self.terminal_group(&worktree, window, cx);
        group.update(cx, |group, cx| {
            group.open(
                crate::ui::terminal_view::Launch {
                    // Un shell et non le programme nu : une tâche est une
                    // ligne de commande du projet, avec ses tubes et ses
                    // redirections, et c'est un shell qui sait les lire.
                    command: Some(("sh".into(), vec!["-lc".into(), line])),
                    env: launch.env.into_iter().collect(),
                    label: SharedString::from(task),
                    agent: false,
                },
                window,
                cx,
            );
        });
        self.show_terminal_panel(window, cx);
    }

    /// Le slug d'un worktree, quand `wt` le connaît.
    fn wt_slug(&self, main: &Path, worktree: &Path) -> Option<String> {
        let root = &self.wt_project(main)?.root;
        let rest = worktree.strip_prefix(root).ok()?;
        let mut parts = rest.components();
        let slug = parts.next()?.as_os_str().to_str()?.to_string();
        parts.next().is_none().then_some(slug)
    }

    // — Intégrer ————————————————————————————————————————————————

    /// Met le worktree à jour depuis sa base : la base a avancé pendant que
    /// l'agent travaillait.
    pub(super) fn update_from_base(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(base) = self.active_review().and_then(|state| state.base.clone()) else {
            return;
        };
        let rebase = Settings::global(cx).update_with_rebase;
        self.git.send(if rebase {
            Cmd::Rebase {
                worktree,
                onto: base,
            }
        } else {
            Cmd::Merge {
                worktree,
                from: base,
                no_ff: false,
            }
        });
        cx.notify();
    }

    /// Intègre la branche d'un worktree dans sa base, depuis le dépôt
    /// principal — le seul endroit d'où la base se met à jour.
    pub(super) fn integrate(&mut self, worktree: PathBuf, cx: &mut Context<Self>) {
        let Some(main) = self.main_of(&worktree) else {
            return;
        };
        let Some(branch) = self
            .repos
            .iter()
            .flat_map(|repo| repo.worktrees.iter())
            .find(|w| w.path == worktree)
            .and_then(|w| w.branch.clone())
        else {
            return;
        };
        let Some(base) = self
            .review
            .get(&worktree)
            .and_then(|state| state.base.clone())
        else {
            return;
        };
        // La base peut être une branche distante (`origin/main`) ; c'est celle
        // du dépôt principal qu'on met à jour, donc son nom court.
        let base = base
            .rsplit_once('/')
            .map(|(_, b)| b)
            .unwrap_or(&base)
            .to_string();
        // Retenu avant l'envoi : c'est à l'arrivée du succès qu'on propose de
        // faire le ménage, et le worktree ne se déduit pas de la réponse.
        self.integrated = Some((worktree, branch.clone()));
        self.git.send(Cmd::Integrate {
            main,
            branch,
            base,
            no_ff: Settings::global(cx).integrate_no_ff,
        });
        cx.notify();
    }

    /// Une intégration a abouti : proposer de retirer le worktree et sa
    /// branche.
    ///
    /// La question se pose parce que `wt` conserve délibérément la branche : ce
    /// qui reste après un `wt rm` est un choix, et c'est à Claudhub de le
    /// demander plutôt qu'à `wt` de le supposer.
    pub(super) fn offer_cleanup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((worktree, branch)) = self.integrated.take() else {
            return;
        };
        let Some(main) = self.main_of(&worktree) else {
            return;
        };
        let label = SharedString::from(format!("{} · {branch}", worktree.display()));
        let entity = cx.entity();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (worktree, branch, main, entity) = (
                worktree.clone(),
                branch.clone(),
                main.clone(),
                entity.clone(),
            );
            dialog
                .title(tr!("worktree-cleanup-title"))
                .child(
                    v_flex()
                        .gap_1()
                        .child(div().text_sm().child(label.clone()))
                        .child(div().text_xs().child(tr!("worktree-cleanup-help"))),
                )
                .confirm()
                .on_ok(move |_, _window, cx| {
                    entity.update(cx, |this, cx| {
                        this.wt_remove(main.clone(), &worktree, cx);
                        this.git.send(Cmd::DeleteBranch {
                            main: main.clone(),
                            name: branch.clone(),
                            force: false,
                        });
                    });
                    true
                })
        });
    }

    /// Le menu contextuel d'un worktree : git d'un côté, le projet de l'autre.
    pub(super) fn worktree_menu(
        &mut self,
        menu: gpui_component::menu::PopupMenu,
        main: PathBuf,
        worktree: PathBuf,
        cx: &mut Context<Self>,
    ) -> gpui_component::menu::PopupMenu {
        self.ensure_wt_project(&main);
        let entity = cx.entity();
        let project = self.wt_project(&main).cloned();
        let known = self.wt_slug(&main, &worktree).is_some();

        let mut menu = {
            let (update, integrate) = (entity.clone(), entity.clone());
            let (for_update, for_integrate) = (worktree.clone(), worktree.clone());
            menu.item(
                PopupMenuItem::new(tr!("worktree-update"))
                    .icon(icon("arrow-down-to-line"))
                    .on_click(move |_, window, cx| {
                        update.update(cx, |this, cx| {
                            this.select_worktree(for_update.clone(), window, cx);
                            this.update_from_base(cx);
                        });
                    }),
            )
            .item(
                PopupMenuItem::new(tr!("worktree-integrate"))
                    .icon(icon("git-merge"))
                    .on_click(move |_, _window, cx| {
                        integrate.update(cx, |this, cx| this.integrate(for_integrate.clone(), cx));
                    }),
            )
        };

        let Some(project) = project.filter(|_| known) else {
            return menu;
        };

        menu = menu.separator();
        if project.has_up {
            let (entity, main, worktree) = (entity.clone(), main.clone(), worktree.clone());
            menu = menu.item(
                PopupMenuItem::new(tr!("worktree-up"))
                    .icon(icon("play"))
                    .on_click(move |_, _window, cx| {
                        entity.update(cx, |this, cx| this.wt_up(main.clone(), &worktree, cx));
                    }),
            );
        }
        if project.has_down {
            let (entity, main, worktree) = (entity.clone(), main.clone(), worktree.clone());
            menu = menu.item(
                PopupMenuItem::new(tr!("worktree-down"))
                    .icon(icon("circle-stop"))
                    .on_click(move |_, _window, cx| {
                        entity.update(cx, |this, cx| this.wt_down(main.clone(), &worktree, cx));
                    }),
            );
        }
        {
            let (entity, main, worktree) = (entity.clone(), main.clone(), worktree.clone());
            menu = menu.item(
                PopupMenuItem::new(tr!("worktree-remove"))
                    .icon(icon("trash-2"))
                    .on_click(move |_, _window, cx| {
                        entity.update(cx, |this, cx| this.wt_remove(main.clone(), &worktree, cx));
                    }),
            );
        }

        // Les tâches du projet, telles qu'il les déclare. Claudhub ne sait pas
        // ce qu'elles font, et c'est le principe.
        if project.tasks.is_empty() {
            return menu;
        }
        menu = menu.separator();
        project.tasks.into_iter().fold(menu, |menu, task| {
            let (entity, main, worktree) = (entity.clone(), main.clone(), worktree.clone());
            let name = task.name.clone();
            let label = if task.description.is_empty() {
                task.name.clone()
            } else {
                format!("{} — {}", task.name, task.description)
            };
            menu.item(
                PopupMenuItem::new(SharedString::from(label))
                    .icon(icon("terminal"))
                    .on_click(move |_, _window, cx| {
                        entity.update(cx, |this, cx| {
                            this.wt_task(main.clone(), worktree.clone(), name.clone(), cx)
                        });
                    }),
            )
        })
    }

    /// Le bouton « ouvrir » d'un worktree, quand le projet expose une adresse.
    pub(super) fn render_wt_links(
        &self,
        worktree: &Path,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let endpoints = self.wt_state(worktree)?.endpoints.clone();
        let first = endpoints.first()?.clone();
        Some(
            Button::new(SharedString::from(format!(
                "wt-open-{}",
                worktree.display()
            )))
            .ghost()
            .xsmall()
            .icon(icon("external-link"))
            .tooltip(SharedString::from(first.label.clone()))
            .on_click(cx.listener(move |_, _, _window, cx| {
                cx.open_url(&first.url);
            })),
        )
    }

    /// La pastille d'état d'un worktree que `wt` sait démarrer.
    pub(super) fn render_wt_state(
        &self,
        worktree: &Path,
        cx: &gpui::App,
    ) -> Option<impl IntoElement> {
        let up = self.wt_state(worktree)?.up?;
        let color = if up {
            cx.theme().success
        } else {
            cx.theme().muted_foreground
        };
        Some(
            h_flex().flex_none().child(
                div()
                    .size(px(7.))
                    .rounded_full()
                    .when(up, |el| el.bg(color))
                    .when(!up, |el| el.border_1().border_color(color.opacity(0.8))),
            ),
        )
    }
}
