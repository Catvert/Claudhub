//! Le panneau Sentry : des issues, leur trace, et de quoi les confier.
//!
//! C'est la boucle complète que ce jalon ferme : du rapport d'erreur au
//! worktree relu. On lit une issue, on clique une frame pour ouvrir le code
//! fautif, et on confie le tout à un agent — soit dans le worktree courant,
//! soit dans un worktree neuf créé pour l'occasion.
//!
//! Claudhub **n'envoie rien** à Sentry : il lit.

use std::path::PathBuf;

use gpui::{div, prelude::*, px, Context, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex, v_flex, ActiveTheme, Disableable, Sizable,
};

use crate::runtime::Cmd;
use crate::sentry::{Event, Issue};
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::settings::Settings;
use crate::ui::store::Store;

/// Ce que le panneau Sentry affiche.
#[derive(Default)]
pub struct SentryState {
    pub issues: Vec<Issue>,
    pub selected: Option<String>,
    /// L'événement de l'issue choisie, quand il est arrivé.
    pub event: Option<Event>,
    /// Une requête est partie et n'est pas revenue. Sans ce garde, chaque
    /// frame du panneau rappellerait l'API.
    pub loading: bool,
    /// Une demande a déjà été faite : le panneau ne recharge pas tout seul, une
    /// API distante n'ayant pas à être interrogée à chaque ouverture d'onglet.
    pub asked: bool,
}

impl ClaudhubApp {
    /// Le projet Sentry du dépôt courant, tel que le magasin le retient.
    ///
    /// Le projet dépend du **dépôt** et non du compte : deux dépôts d'une même
    /// organisation n'ont pas les mêmes erreurs.
    fn sentry_project(&self, cx: &gpui::App) -> Option<String> {
        let main = self.main_of(self.active.as_deref()?)?;
        Store::global(cx)
            .repos
            .get(&main)
            .and_then(|repo| repo.sentry_project.clone())
            .filter(|project| !project.trim().is_empty())
    }

    pub(super) fn set_sentry_project(&mut self, project: String, cx: &mut Context<Self>) {
        let Some(main) = self.active.as_deref().and_then(|w| self.main_of(w)) else {
            return;
        };
        let project = project.trim().to_string();
        Store::update_global(cx, |store| {
            store.repos.entry(main.clone()).or_default().sentry_project =
                (!project.is_empty()).then_some(project.clone());
        });
        self.sentry.asked = false;
        self.load_issues(cx);
    }

    pub(super) fn load_issues(&mut self, cx: &mut Context<Self>) {
        let settings = Settings::global(cx);
        let (org, query) = (
            settings.sentry_org.trim().to_string(),
            settings.sentry_query.clone(),
        );
        let Some(project) = self.sentry_project(cx) else {
            return;
        };
        if org.is_empty() || self.sentry.loading {
            return;
        }
        self.sentry.loading = true;
        self.sentry.asked = true;
        self.git.send(Cmd::LoadIssues {
            org,
            project,
            query,
        });
        cx.notify();
    }

    pub(super) fn issues_arrived(&mut self, issues: Vec<Issue>, cx: &mut Context<Self>) {
        self.sentry.loading = false;
        self.sentry.issues = issues;
        cx.notify();
    }

    pub(super) fn issue_event_arrived(
        &mut self,
        issue: String,
        event: Event,
        cx: &mut Context<Self>,
    ) {
        // Une réponse en retard, pour une issue qu'on ne regarde plus,
        // remplacerait la trace par la mauvaise.
        if self.sentry.selected.as_deref() == Some(issue.as_str()) {
            self.sentry.event = Some(event);
            cx.notify();
        }
    }

    pub(super) fn select_issue(&mut self, id: String, cx: &mut Context<Self>) {
        if self.sentry.selected.as_deref() == Some(id.as_str()) {
            return;
        }
        self.sentry.selected = Some(id.clone());
        self.sentry.event = None;
        self.git.send(Cmd::LoadIssueEvent { issue: id });
        cx.notify();
    }

    /// Le prompt d'une issue, quand sa trace est là.
    fn issue_prompt(&self) -> Option<String> {
        let worktree = self.active.as_deref()?;
        let event = self.sentry.event.as_ref()?;
        let id = self.sentry.selected.as_deref()?;
        let issue = self.sentry.issues.iter().find(|issue| issue.id == id)?;
        Some(crate::sentry::prompt(issue, event, worktree))
    }

    /// Confie l'issue à l'agent du worktree courant.
    pub(super) fn hand_issue_to_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(text), Some(worktree)) = (self.issue_prompt(), self.active.clone()) else {
            return;
        };
        let group = self.terminal_group(&worktree, window, cx);
        group.update(cx, |group, cx| group.send_to_agent(text, window, cx));
        self.show_terminal_panel(window, cx);
    }

    /// Ouvre un worktree pour cette issue, et y démarre l'agent avec le
    /// rapport.
    ///
    /// C'est la boucle complète : du rapport d'erreur au worktree relu. La
    /// création passe par `wt`, donc par les copies, les ports et les hooks du
    /// projet — un worktree où l'agent peut travailler tout de suite.
    pub(super) fn open_worktree_for_issue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(text), Some(id)) = (self.issue_prompt(), self.sentry.selected.clone()) else {
            return;
        };
        let Some(main) = self.active.as_deref().and_then(|w| self.main_of(w)) else {
            return;
        };
        let slug = format!("sentry-{id}");
        // Le worktree n'existe pas encore : le prompt est retenu et livré à
        // l'arrivée de la liste des worktrees, quand `wt` a fini ses hooks.
        if let Some(root) = self.wt_project(&main).map(|project| project.root.clone()) {
            self.awaiting_agent = Some((root.join(&slug), text));
        }
        self.start_worktree(main, slug, None, window, cx);
    }

    /// Livre le prompt en attente, si le worktree qu'il vise vient d'arriver.
    ///
    /// Appelé à chaque `Evt::Worktrees` : c'est le seul signal qui dise que
    /// `wt` a terminé — la création lance des hooks qui durent des minutes, et
    /// rien d'autre ne marque leur fin.
    pub(super) fn deliver_awaited_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((path, _)) = self.awaiting_agent.as_ref() else {
            return;
        };
        let path = path.clone();
        if !self
            .repos
            .iter()
            .flat_map(|repo| repo.worktrees.iter())
            .any(|w| w.path == path)
        {
            return;
        }
        let Some((_, text)) = self.awaiting_agent.take() else {
            return;
        };
        self.select_worktree(path.clone(), window, cx);
        let group = self.terminal_group(&path, window, cx);
        group.update(cx, |group, cx| group.send_to_agent(text, window, cx));
        self.show_terminal_panel(window, cx);
    }

    /// Ouvre le fichier d'une frame, à sa ligne.
    ///
    /// L'éditeur externe d'abord, s'il est configuré : c'est lui qui sait
    /// ouvrir à une ligne. À défaut, l'éditeur intégré, qui ouvre au moins le
    /// bon fichier.
    fn open_frame(&mut self, path: PathBuf, line: usize, cx: &mut Context<Self>) {
        if Settings::global(cx).external_editor.trim().is_empty() {
            self.open_in_editor(path, cx);
        } else {
            self.open_externally(path, line, cx);
        }
    }

    pub(super) fn render_sentry(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let org = Settings::global(cx).sentry_org.trim().to_string();
        let project = self.sentry_project(cx);
        let muted = cx.theme().muted_foreground;

        let bar =
            h_flex()
                .h(crate::ui::theme::bar_height(cx))
                .w_full()
                .px_2()
                .gap_2()
                .items_center()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(icon("triangle-alert").xsmall())
                .child(div().flex_1().truncate().text_xs().text_color(muted).child(
                    SharedString::from(match &project {
                        Some(project) => format!("{org}/{project}"),
                        None => tr!("sentry-no-project").to_string(),
                    }),
                ))
                .child(
                    Button::new("sentry-project")
                        .ghost()
                        .xsmall()
                        .icon(icon("settings"))
                        .tooltip(tr!("sentry-set-project"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.prompt_sentry_project(window, cx)
                        })),
                )
                .child(
                    Button::new("sentry-reload")
                        .ghost()
                        .xsmall()
                        .icon(icon("refresh-cw"))
                        .tooltip(tr!("action-refresh"))
                        .disabled(self.sentry.loading)
                        .on_click(cx.listener(|this, _, _window, cx| {
                            this.sentry.loading = false;
                            this.load_issues(cx);
                        })),
                );

        if org.is_empty() || project.is_none() {
            return v_flex()
                .size_full()
                .child(bar)
                .child(empty(tr!("sentry-configure"), cx))
                .into_any_element();
        }
        // Le premier chargement se fait au rendu, une seule fois : une API
        // distante n'a pas à être interrogée à chaque ouverture d'onglet, ni
        // à chaque frame.
        if !self.sentry.asked {
            self.load_issues(cx);
        }

        let issues = self.sentry.issues.clone();
        let selected = self.sentry.selected.clone();
        if issues.is_empty() {
            return v_flex()
                .size_full()
                .child(bar)
                .child(empty(
                    if self.sentry.loading {
                        tr!("sentry-loading")
                    } else {
                        tr!("sentry-empty")
                    },
                    cx,
                ))
                .into_any_element();
        }

        let mut rows = Vec::new();
        for (index, issue) in issues.into_iter().enumerate() {
            rows.push(self.render_issue(index, issue, selected.as_deref(), cx));
        }

        v_flex()
            .size_full()
            .child(bar)
            .child(
                v_flex()
                    .id("sentry-issues")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(rows),
            )
            .children(self.render_trace(cx))
            .into_any_element()
    }

    fn render_issue(
        &mut self,
        index: usize,
        issue: Issue,
        selected: Option<&str>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_selected = selected == Some(issue.id.as_str());
        let muted = cx.theme().muted_foreground;
        let color = match issue.level.as_str() {
            "fatal" | "error" => cx.theme().danger,
            "warning" => cx.theme().warning,
            _ => muted,
        };
        let id = issue.id.clone();
        v_flex()
            .id(("issue", index))
            .px_2()
            .py_1()
            .gap_1()
            .cursor_pointer()
            .border_b_1()
            .border_color(cx.theme().border)
            .when(is_selected, |el| el.bg(cx.theme().accent))
            .hover(|s| s.bg(cx.theme().accent.opacity(0.4)))
            .on_click(cx.listener(move |this, _, _window, cx| {
                this.select_issue(id.clone(), cx);
            }))
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(div().size(px(6.)).rounded_full().bg(color))
                    .child(div().flex_1().truncate().text_sm().child(issue.title))
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(issue.count.to_string()),
                    ),
            )
            .when(!issue.culprit.is_empty(), |el| {
                el.child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(muted)
                        .child(issue.culprit),
                )
            })
    }

    /// La trace de l'issue choisie, avec ses frames cliquables.
    fn render_trace(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let worktree = self.active.clone()?;
        let event = self.sentry.event.clone()?;
        let (muted, mono) = (
            cx.theme().muted_foreground,
            cx.theme().mono_font_family.clone(),
        );
        let has_prompt = self.issue_prompt().is_some();

        let mut frames = Vec::new();
        for (index, frame) in event.frames.iter().enumerate() {
            let path = frame.repo_path(&worktree);
            let line = frame.line;
            let target = PathBuf::from(&path);
            let label = SharedString::from(if frame.function.is_empty() {
                format!("{path}:{line}")
            } else {
                format!("{path}:{line} · {}", frame.function)
            });
            frames.push(
                div()
                    .id(("frame", index))
                    .px_2()
                    .py_0p5()
                    .truncate()
                    .text_xs()
                    .font_family(mono.clone())
                    .cursor_pointer()
                    // Les frames de l'application ressortent : c'est là qu'est
                    // le bug, le reste est le chemin qui y a mené.
                    .when(!frame.in_app, |el| el.text_color(muted))
                    .hover(|s| s.bg(cx.theme().accent.opacity(0.4)))
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.open_frame(target.clone(), line.max(1), cx);
                    }))
                    .child(label)
                    .into_any_element(),
            );
        }

        Some(
            v_flex()
                .max_h(px(260.))
                .border_t_1()
                .border_color(cx.theme().border)
                .child(
                    h_flex()
                        .px_2()
                        .py_1()
                        .gap_1()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .truncate()
                                .text_xs()
                                .text_color(muted)
                                .child(SharedString::from(event.message.clone())),
                        )
                        .child(
                            Button::new("sentry-hand")
                                .outline()
                                .xsmall()
                                .label(tr!("sentry-hand-to-agent"))
                                .disabled(!has_prompt)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.hand_issue_to_agent(window, cx)
                                })),
                        )
                        .child(
                            Button::new("sentry-worktree")
                                .ghost()
                                .xsmall()
                                .label(tr!("sentry-open-worktree"))
                                .disabled(!has_prompt)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_worktree_for_issue(window, cx)
                                })),
                        ),
                )
                .child(
                    v_flex()
                        .id("sentry-trace")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .children(frames),
                ),
        )
    }

    fn prompt_sentry_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.sentry_project(cx).unwrap_or_default();
        self.open_text_dialog(
            tr!("sentry-set-project"),
            SharedString::from(current),
            window,
            cx,
            |this, value, _window, cx| this.set_sentry_project(value, cx),
        );
    }
}

fn empty(message: SharedString, cx: &Context<ClaudhubApp>) -> impl IntoElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .text_color(cx.theme().muted_foreground)
        .child(icon("triangle-alert"))
        .child(div().text_sm().px_4().child(message))
}
