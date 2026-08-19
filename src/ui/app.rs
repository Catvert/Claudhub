//! L'entité racine : l'état de la fenêtre et la pompe d'événements.
//!
//! Les sous-vues ne sont pas des entités séparées mais des `impl PerchApp`
//! répartis par fichier (`sidebar`, `review`, `branches`). Tout ce qu'elles
//! affichent vient du même état, et le faire circuler entre entités coûterait
//! plus de code qu'il n'en économise. Les terminaux font exception : ils ont
//! leur propre cycle de vie et sont des `Entity<TerminalView>`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui::{
    div, prelude::*, px, App, Context, Entity, FocusHandle, Focusable, Render, SharedString, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::InputState,
    resizable::{h_resizable, resizable_panel, v_resizable, ResizableState},
    v_flex, ActiveTheme, Disableable, Root, Selectable, Sizable, StyledExt, WindowExt,
};

use crate::git::{Branch, DiffFile, DiffRange, Status, Worktree};
use crate::runtime::watch::Watcher;
use crate::runtime::{self, Action, Cmd, Evt};
use crate::tr;
use crate::ui::diff_view::Rendered;
use crate::ui::icons::icon;
use crate::ui::settings::Settings;
use crate::ui::terminal_view::TerminalGroup;

/// Un dépôt ouvert dans la barre latérale.
pub struct RepoState {
    pub main: PathBuf,
    pub name: String,
    pub worktrees: Vec<Worktree>,
    pub branches: Vec<Branch>,
    /// Branche d'intégration, telle que git la déclare. Elle n'est connue
    /// qu'après la réponse du worker : jusque-là, la revue de branche n'a pas
    /// de base et son onglet reste inactif — proposer un `main` supposé
    /// produirait un « unknown revision » sur tout dépôt qui ne s'appelle pas
    /// ainsi.
    pub default_base: Option<String>,
    pub collapsed: bool,
}

/// Ce que la revue montre pour un worktree donné.
///
/// Un état par worktree, et non un seul état global : passer d'un worktree à
/// l'autre pour comparer est le geste central de l'outil, et il ne doit pas
/// coûter la perte du fichier qu'on était en train de lire.
pub struct ReviewState {
    pub range: DiffRange,
    pub status: Status,
    pub files: Vec<DiffFile>,
    pub selected: Option<PathBuf>,
    /// Le diff affiché, avec tout ce qui s'en déduit. Un `Rc` parce que le
    /// rendu doit le capturer dans la fermeture de la liste virtualisée, et
    /// qu'en copier plusieurs milliers de lignes par frame reviendrait à
    /// annuler le bénéfice de la virtualisation.
    pub diff: Option<std::rc::Rc<Rendered>>,
    /// Base de comparaison de la revue de branche, devinée à l'ouverture.
    pub base: Option<String>,
}

impl Default for ReviewState {
    fn default() -> Self {
        Self {
            range: DiffRange::Unstaged,
            status: Status::default(),
            files: Vec::new(),
            selected: None,
            diff: None,
            base: None,
        }
    }
}

/// Le résultat de la dernière action, affiché dans la barre d'état.
pub struct Toast {
    pub text: SharedString,
    pub error: bool,
}

pub struct PerchApp {
    pub(super) settings: Settings,
    pub(super) git: runtime::Handle,
    pub(super) repos: Vec<RepoState>,
    /// Worktree sélectionné : la clé de presque tout le reste.
    pub(super) active: Option<PathBuf>,
    pub(super) review: HashMap<PathBuf, ReviewState>,
    pub(super) terminals: HashMap<PathBuf, Entity<TerminalGroup>>,
    pub(super) commit_input: Entity<InputState>,
    pub(super) toast: Option<Toast>,
    pub(super) show_terminal: bool,
    pub(super) show_branches: bool,

    /// Surveillance du worktree affiché. `None` si le système refuse de nous
    /// donner un observateur (limite d'inotify atteinte, par exemple) : Perch
    /// marche encore, il faut seulement actualiser à la main.
    watcher: Option<Watcher>,

    /// Défilement de la liste virtualisée du diff. Il vit sur la vue et n'est
    /// jamais reconstruit : le recréer par frame remettrait le diff en haut à
    /// chaque image.
    pub(super) diff_scroll: gpui::UniformListScrollHandle,

    sidebar_resize: Entity<ResizableState>,
    center_resize: Entity<ResizableState>,
    bottom_resize: Entity<ResizableState>,
    focus: FocusHandle,
}

impl PerchApp {
    pub fn new(settings: Settings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (git, events) = runtime::spawn();

        let commit_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .placeholder(tr!("commit-placeholder"))
        });

        let mut app = Self {
            settings,
            git,
            repos: Vec::new(),
            active: None,
            review: HashMap::new(),
            terminals: HashMap::new(),
            commit_input,
            toast: None,
            show_terminal: true,
            show_branches: false,
            watcher: None,
            diff_scroll: gpui::UniformListScrollHandle::new(),
            sidebar_resize: cx.new(|_| ResizableState::default()),
            center_resize: cx.new(|_| ResizableState::default()),
            bottom_resize: cx.new(|_| ResizableState::default()),
            focus: cx.focus_handle(),
        };

        app.pump_events(events, window, cx);
        app.start_watching(window, cx);

        // Les dépôts de la session précédente, puis le répertoire courant s'il
        // en est un — c'est ce qu'attend quelqu'un qui lance `perch` depuis son
        // projet.
        let remembered = app.settings.repositories.clone();
        for path in remembered {
            app.git.send(Cmd::OpenRepo(path));
        }
        if let Ok(cwd) = std::env::current_dir() {
            if crate::git::repo::is_repo(&cwd) {
                app.git.send(Cmd::OpenRepo(cwd));
            }
        }
        app
    }

    /// Draine les événements des workers par lots.
    ///
    /// Par lots parce qu'un `update_in` par événement force un cycle d'effets
    /// gpui à chaque fois : une ouverture de dépôt qui en produit une dizaine
    /// coûterait dix rendus au lieu d'un.
    fn pump_events(
        &mut self,
        events: async_channel::Receiver<Evt>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        const BATCH: usize = 64;
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(evt) = events.recv().await {
                let mut batch = vec![evt];
                while batch.len() < BATCH {
                    let Ok(next) = events.try_recv() else { break };
                    batch.push(next);
                }
                let alive = this
                    .update_in(cx, |app, window, cx| {
                        for evt in batch {
                            app.handle_event(evt, window, cx);
                        }
                    })
                    .is_ok();
                if !alive {
                    break; // la fenêtre est fermée
                }
            }
        })
        .detach();
    }

    /// Branche la surveillance de fichiers sur le rafraîchissement du statut.
    ///
    /// Le chemin reçu est rattaché au worktree ouvert qui le contient : le
    /// surveillant ne connaît que des fichiers, l'application seule sait à
    /// quel checkout ils appartiennent.
    fn start_watching(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (watcher, changes) = match Watcher::new() {
            Ok(pair) => pair,
            Err(e) => {
                log::warn!("surveillance des fichiers indisponible : {e:#}");
                return;
            }
        };
        self.watcher = Some(watcher);

        cx.spawn_in(window, async move |this, cx| {
            while let Ok(path) = changes.recv().await {
                let alive = this
                    .update(cx, |app, cx| app.file_changed(&path, cx))
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    fn file_changed(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(active) = self.active.clone() else {
            return;
        };
        // Un worktree lié vit à l'intérieur d'un autre chez certains agencements ;
        // ne réagir que pour le checkout affiché évite un rafraîchissement en
        // double et une liste qui clignote.
        if !path.starts_with(&active) {
            return;
        }
        self.git.send(Cmd::RefreshStatus { worktree: active });
        cx.notify();
    }

    fn handle_event(&mut self, evt: Evt, window: &mut Window, cx: &mut Context<Self>) {
        match evt {
            Evt::RepoOpened {
                main,
                name,
                worktrees,
            } => {
                if self.repos.iter().any(|r| r.main == main) {
                    return; // déjà ouvert : rouvrir ne doit pas dupliquer
                }
                let first = worktrees.first().map(|w| w.path.clone());
                self.repos.push(RepoState {
                    main: main.clone(),
                    name,
                    worktrees,
                    branches: Vec::new(),
                    default_base: None,
                    collapsed: false,
                });
                self.settings.remember_repository(&main);
                self.settings.save();
                self.git.send(Cmd::LoadBranches { main });
                if self.active.is_none() {
                    if let Some(path) = first {
                        self.select_worktree(path, cx);
                    }
                }
            }
            Evt::Worktrees { main, worktrees } => {
                if let Some(repo) = self.repos.iter_mut().find(|r| r.main == main) {
                    repo.worktrees = worktrees;
                }
                // Le worktree actif peut avoir été retiré sous nos pieds.
                if let Some(active) = self.active.clone() {
                    if !self.worktree_exists(&active) {
                        self.active = None;
                        self.review.remove(&active);
                        self.terminals.remove(&active);
                        if let Some(first) = self.first_worktree() {
                            self.select_worktree(first, cx);
                        }
                    }
                }
            }
            Evt::Status { worktree, status } => {
                let base = self.default_base_for(&worktree);
                let state = self.review.entry(worktree.clone()).or_default();
                state.status = status;
                if state.base.is_none() {
                    state.base = base;
                }
                // La liste des fichiers de la revue courante dépend du statut :
                // la recharger ici évite que la vue affiche un fichier qui
                // vient d'être indexé du mauvais côté.
                let range = state.range.clone();
                self.git.send(Cmd::LoadDiffFiles { worktree, range });
            }
            Evt::DiffFiles {
                worktree,
                range,
                files,
            } => {
                let Some(state) = self.review.get_mut(&worktree) else {
                    return;
                };
                // Une réponse en retard, pour une portée qu'on ne regarde
                // plus, remplacerait la liste par la mauvaise.
                if state.range != range {
                    return;
                }
                let still_there = state
                    .selected
                    .as_ref()
                    .is_some_and(|p| files.iter().any(|f| &f.path == p));
                state.files = files;
                if !still_there {
                    state.selected = None;
                    state.diff = None;
                    let next = state.files.first().map(|f| f.path.clone());
                    if let Some(path) = next {
                        self.open_file(worktree, path, cx);
                    }
                }
            }
            Evt::FileDiff {
                worktree,
                path,
                diff,
            } => {
                // Le thème est lu avant l'emprunt mutable de l'état : la
                // coloration en dépend, et `cx.theme()` emprunte `cx`.
                let theme = cx.theme().highlight_theme.clone();
                if let Some(state) = self.review.get_mut(&worktree) {
                    if state.selected.as_deref() == Some(path.as_path()) {
                        state.diff = Some(std::rc::Rc::new(Rendered::new(&path, diff, &theme)));
                    }
                }
            }
            Evt::Branches {
                main,
                branches,
                default_base,
            } => {
                if let Some(repo) = self.repos.iter_mut().find(|r| r.main == main) {
                    repo.branches = branches;
                    repo.default_base = default_base;
                }
                // Les revues déjà ouvertes attendaient peut-être cette base :
                // le statut arrive avant les branches, et rien ne les
                // rafraîchira une seconde fois.
                let bases: Vec<(PathBuf, Option<String>)> = self
                    .review
                    .keys()
                    .map(|worktree| (worktree.clone(), self.default_base_for(worktree)))
                    .collect();
                for (worktree, base) in bases {
                    if let Some(state) = self.review.get_mut(&worktree) {
                        if state.base.is_none() {
                            state.base = base;
                        }
                    }
                }
            }
            Evt::Done {
                worktree,
                action,
                output,
            } => {
                if action == Action::Commit {
                    self.commit_input.update(cx, |input, cx| {
                        input.set_value("", window, cx);
                    });
                }
                let text = if output.trim().is_empty() {
                    tr!(action.success_key())
                } else {
                    SharedString::from(output.trim().to_string())
                };
                self.toast = Some(Toast { text, error: false });
                // Une opération qui a bougé HEAD change aussi les branches.
                if matches!(
                    action,
                    Action::Commit | Action::Fetch | Action::Pull | Action::Push | Action::Checkout
                ) {
                    if let Some(main) = worktree.as_deref().and_then(|w| self.main_of(w)) {
                        self.git.send(Cmd::LoadBranches { main });
                    }
                }
            }
            Evt::Failed {
                action, message, ..
            } => {
                log::warn!("{action:?} a échoué : {message}");
                self.toast = Some(Toast {
                    text: SharedString::from(message),
                    error: true,
                });
            }
        }
        cx.notify();
    }

    // — Sélection ————————————————————————————————————————————————

    pub(super) fn select_worktree(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.active.as_deref() == Some(path.as_path()) {
            return;
        }
        if let Some(watcher) = self.watcher.as_mut() {
            if let Some(previous) = self.active.as_deref() {
                watcher.unwatch(previous);
            }
            watcher.watch(&path);
        }
        self.active = Some(path.clone());
        self.review.entry(path.clone()).or_default();
        self.git.send(Cmd::RefreshStatus { worktree: path });
        cx.notify();
    }

    pub(super) fn open_file(&mut self, worktree: PathBuf, path: PathBuf, cx: &mut Context<Self>) {
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        state.selected = Some(path.clone());
        // Le diff précédent est effacé tout de suite : garder celui d'un autre
        // fichier le temps de la lecture donnerait l'impression que le clic
        // n'a rien fait, puis que le contenu change tout seul.
        state.diff = None;
        let range = state.range.clone();
        let untracked = state
            .status
            .files
            .iter()
            .any(|f| f.path == path && f.is_untracked());
        self.git.send(Cmd::LoadFileDiff {
            worktree,
            range,
            path,
            context: self.settings.diff_context,
            untracked,
        });
        cx.notify();
    }

    pub(super) fn set_range(&mut self, range: DiffRange, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let Some(state) = self.review.get_mut(&worktree) else {
            return;
        };
        if state.range == range {
            return;
        }
        state.range = range.clone();
        state.files.clear();
        state.selected = None;
        state.diff = None;
        self.git.send(Cmd::LoadDiffFiles { worktree, range });
        cx.notify();
    }

    pub(super) fn refresh_active(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self.active.clone() else {
            return;
        };
        if let Some(main) = self.main_of(&worktree) {
            self.git.send(Cmd::RefreshRepo { main: main.clone() });
            self.git.send(Cmd::LoadBranches { main });
        }
        self.git.send(Cmd::RefreshStatus { worktree });
        cx.notify();
    }

    // — Accès à l'état ——————————————————————————————————————————

    pub(super) fn active_review(&self) -> Option<&ReviewState> {
        self.active.as_ref().and_then(|p| self.review.get(p))
    }

    pub(super) fn main_of(&self, worktree: &Path) -> Option<PathBuf> {
        self.repos
            .iter()
            .find(|r| r.worktrees.iter().any(|w| w.path == worktree))
            .map(|r| r.main.clone())
    }

    pub(super) fn repo_of(&self, worktree: &Path) -> Option<&RepoState> {
        self.repos
            .iter()
            .find(|r| r.worktrees.iter().any(|w| w.path == worktree))
    }

    pub(super) fn active_worktree(&self) -> Option<&Worktree> {
        let path = self.active.as_deref()?;
        self.repos
            .iter()
            .flat_map(|r| r.worktrees.iter())
            .find(|w| w.path == path)
    }

    fn worktree_exists(&self, path: &Path) -> bool {
        self.repos
            .iter()
            .any(|r| r.worktrees.iter().any(|w| w.path == path))
    }

    fn first_worktree(&self) -> Option<PathBuf> {
        self.repos
            .iter()
            .flat_map(|r| r.worktrees.iter())
            .next()
            .map(|w| w.path.clone())
    }

    /// Base de comparaison d'un worktree : la branche d'intégration du dépôt,
    /// sauf quand c'est justement celle qui y est déployée — comparer une
    /// branche à elle-même ne montre rien.
    fn default_base_for(&self, worktree: &Path) -> Option<String> {
        let repo = self.repo_of(worktree)?;
        let base = repo.default_base.as_deref()?;
        let current = repo
            .worktrees
            .iter()
            .find(|w| w.path == worktree)
            .and_then(|w| w.branch.as_deref());
        (Some(base) != current).then(|| base.to_string())
    }

    // — Rendu ——————————————————————————————————————————————————

    fn render_topbar(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let worktree = self.active_worktree();
        let branch = worktree
            .and_then(|w| w.branch.clone())
            .unwrap_or_else(|| tr!("branch-detached").to_string());
        let label = worktree
            .map(|w| w.label())
            .unwrap_or_else(|| tr!("no-worktree").to_string());
        let has_active = self.active.is_some();
        let review = self.active_review();
        let (ahead, behind) = review
            .map(|r| (r.status.ahead, r.status.behind))
            .unwrap_or((0, 0));

        h_flex()
            .h(px(38.))
            .w_full()
            .px_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().title_bar)
            .child(icon("git-branch").small())
            .child(div().font_semibold().text_sm().child(label))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(branch),
            )
            .when(behind > 0, |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("↓{behind}")),
                )
            })
            .when(ahead > 0, |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("↑{ahead}")),
                )
            })
            .child(div().flex_1())
            .child(
                Button::new("fetch")
                    .ghost()
                    .small()
                    .icon(icon("refresh-cw"))
                    .tooltip(tr!("action-fetch"))
                    .disabled(!has_active)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(worktree) = this.active.clone() {
                            this.git.send(Cmd::Fetch { worktree });
                        }
                        cx.notify();
                    })),
            )
            .child(
                Button::new("pull")
                    .ghost()
                    .small()
                    .icon(icon("arrow-down-to-line"))
                    .tooltip(tr!("action-pull"))
                    .disabled(!has_active)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(worktree) = this.active.clone() {
                            this.git.send(Cmd::Pull { worktree });
                        }
                        cx.notify();
                    })),
            )
            .child(
                Button::new("push")
                    .ghost()
                    .small()
                    .icon(icon("arrow-up-from-line"))
                    .tooltip(tr!("action-push"))
                    .disabled(!has_active)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(worktree) = this.active.clone() {
                            this.git.send(Cmd::Push {
                                worktree,
                                force_with_lease: false,
                            });
                        }
                        cx.notify();
                    })),
            )
            .child(
                Button::new("agent")
                    .ghost()
                    .small()
                    .icon(icon("bot"))
                    .tooltip(tr!("terminal-agent"))
                    .disabled(!has_active)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_agent_terminal(window, cx);
                    })),
            )
            .child(
                Button::new("agent")
                    .ghost()
                    .small()
                    .icon(icon("bot"))
                    .tooltip(tr!("terminal-agent"))
                    .disabled(!has_active)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_agent_terminal(window, cx);
                    })),
            )
            .child(
                Button::new("branches")
                    .ghost()
                    .small()
                    .icon(icon("git-merge"))
                    .tooltip(tr!("panel-branches"))
                    .selected(self.show_branches)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_branches = !this.show_branches;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("terminal")
                    .ghost()
                    .small()
                    .icon(icon("square-terminal"))
                    .tooltip(tr!("panel-terminal"))
                    .selected(self.show_terminal)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_terminal = !this.show_terminal;
                        cx.notify();
                    })),
            )
    }

    fn render_status_bar(&self, cx: &Context<Self>) -> impl IntoElement {
        let (text, error) = match &self.toast {
            Some(t) => (t.text.clone(), t.error),
            None => (SharedString::default(), false),
        };
        h_flex()
            .h(px(24.))
            .w_full()
            .px_2()
            .items_center()
            .gap_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().title_bar)
            .text_xs()
            .text_color(if error {
                cx.theme().danger
            } else {
                cx.theme().muted_foreground
            })
            .child(div().truncate().child(text))
    }
}

impl Focusable for PerchApp {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for PerchApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let center = h_resizable("perch-center")
            .with_state(&self.center_resize)
            .child(
                resizable_panel()
                    .size(px(320.))
                    .size_range(px(220.)..px(520.))
                    .child(self.render_file_list(window, cx).into_any_element()),
            )
            .child(resizable_panel().child(self.render_diff(window, cx).into_any_element()));

        let main = h_resizable("perch-main")
            .with_state(&self.sidebar_resize)
            .child(
                resizable_panel()
                    .size(px(260.))
                    .size_range(px(180.)..px(420.))
                    .child(self.render_sidebar(window, cx).into_any_element()),
            )
            .child(resizable_panel().child(if self.show_terminal {
                v_resizable("perch-vertical")
                    .with_state(&self.bottom_resize)
                    .child(resizable_panel().child(center.into_any_element()))
                    .child(
                        resizable_panel()
                            .size(px(280.))
                            .size_range(px(120.)..px(900.))
                            .child(self.render_terminals(window, cx).into_any_element()),
                    )
                    .into_any_element()
            } else {
                center.into_any_element()
            }));

        v_flex()
            .key_context(super::shortcuts::context())
            .track_focus(&self.focus)
            .on_action(cx.listener(super::shortcuts::refresh))
            .on_action(cx.listener(super::shortcuts::new_terminal))
            .on_action(cx.listener(super::shortcuts::close_terminal))
            .on_action(cx.listener(super::shortcuts::toggle_terminal))
            .on_action(cx.listener(super::shortcuts::next_terminal))
            .on_action(cx.listener(super::shortcuts::commit))
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_topbar(window, cx))
            .child(div().flex_1().min_h_0().child(main))
            .child(self.render_status_bar(cx))
            // Les couches de gpui-component doivent être ré-émises par la vue
            // racine, sinon dialogues et notifications ne s'affichent nulle
            // part.
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}

impl PerchApp {
    /// Ouvre un dialogue à une seule ligne de saisie.
    ///
    /// L'`InputState` est créé ici et capturé par la fermeture : une entité
    /// recréée à chaque frame perdrait le curseur, la sélection et le texte
    /// dès le premier caractère.
    pub(super) fn open_text_dialog(
        &mut self,
        title: SharedString,
        placeholder: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_ok: impl Fn(&mut Self, String, &mut Context<Self>) + 'static,
    ) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let entity = cx.entity();
        let on_ok = std::rc::Rc::new(on_ok);
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (input, entity, on_ok) = (input.clone(), entity.clone(), on_ok.clone());
            dialog
                .title(title.clone())
                .confirm()
                .child(gpui_component::input::Input::new(&input))
                .on_ok(move |_, _window, cx| {
                    let value = input.read(cx).value().to_string();
                    entity.update(cx, |this, cx| on_ok(this, value, cx));
                    true
                })
        });
    }
}

impl PerchApp {
    pub(super) fn active_path(&self) -> Option<PathBuf> {
        self.active.clone()
    }

    pub(super) fn show_terminal_panel(&mut self, cx: &mut Context<Self>) {
        self.show_terminal = true;
        cx.notify();
    }

    pub(super) fn toggle_terminal_panel(&mut self, cx: &mut Context<Self>) {
        self.show_terminal = !self.show_terminal;
        cx.notify();
    }
}
