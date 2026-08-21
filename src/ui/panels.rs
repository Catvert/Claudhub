//! Les panneaux du dock.
//!
//! Chaque zone de l'interface est une entité à part, ce que le dock de
//! gpui-component exige pour la déplacer : c'est lui qui gère le glissement,
//! les onglets et les zones d'accueil. Les panneaux ne portent aucun état —
//! ils délèguent à `ClaudhubApp`, qui reste la seule source.
//!
//! La référence à `ClaudhubApp` est **faible**. Forte, elle formerait un cycle —
//! l'application tient le dock, qui tient les panneaux — et rien ne serait
//! libéré à la fermeture de la fenêtre.
//!
//! Rendre depuis un `update` sur `ClaudhubApp` est licite parce que le rendu d'une
//! vue enfant a lieu *après* que la fermeture de rendu du parent a rendu la
//! main : la mise en page est faite hors de cet emprunt.

use gpui::{
    div, prelude::*, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, Render, WeakEntity, Window,
};
use gpui_component::dock::{BasePanel, Panel, PanelControl, PanelEvent};
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::ActiveTheme;

use gpui_component::dock::{panel_handle, register_panel};

use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::find::Pane;
use crate::ui::settings::Settings;

/// Le fond d'un panneau, et les coins bas de la carte qui le porte.
///
/// Plus de carte ici : c'est le **cadre du groupe** qui l'est désormais — le
/// fork arrondit `TabGroupSkin::frame` et espace les splits d'une gouttière,
/// si bien que la barre d'onglets et le contenu partagent la même surface,
/// sans couture ni bordure entre eux. Redessiner une carte à l'intérieur
/// remettrait la couture qu'on vient d'enlever.
///
/// `rounded_b` : le masque de contenu de gpui est **rectangulaire** —
/// l'arrondi du cadre du groupe ne rogne pas ses enfants, et un fond carré
/// peint ici couvrirait les coins bas de la carte. En haut, le rail des
/// onglets est en retrait et laisse le cadre paraître ; en bas, c'est ce
/// fond-ci qui a le dernier mot. Tout panneau doit donc passer par là : celui
/// qui s'en dispense a des coins carrés, et rien ne le signale.
fn pane_frame(content: impl IntoElement, cx: &App) -> gpui::Div {
    div()
        .size_full()
        .rounded_b(cx.theme().radius_lg)
        .bg(cx.theme().background)
        .child(content)
}

/// Idem, en notant le panneau qu'on vient de **toucher** : c'est ce qui donne
/// une cible à `Ctrl+F`.
///
/// Le clic et non le focus : le dock pose le focus sur l'onglet actif de
/// **chaque** zone, il y en a trois affichées en même temps, et rien là-dedans
/// ne dit laquelle l'utilisateur regarde. En phase de **capture**, donc avant
/// les enfants et sans qu'aucun d'eux puisse l'arrêter : une ligne de diff
/// comme une case à cocher consomment leur clic, et le panneau ne saurait
/// jamais qu'on l'a touché.
///
/// Les terminaux n'ont pas de panneau de recherche — `Ctrl+F` y appartient au
/// programme qui tourne —, et c'est pour eux que les deux fonctions sont
/// séparées : le cadre leur revient, la note non.
fn pane_root(
    app: &Entity<ClaudhubApp>,
    pane: Pane,
    content: impl IntoElement,
    cx: &App,
) -> impl IntoElement {
    let app = app.clone();
    pane_frame(content, cx).capture_any_mouse_down(move |_, _window, cx| {
        app.update(cx, |app, cx| app.touch_pane(pane, cx));
    })
}

/// Déclare les panneaux au registre du dock.
///
/// C'est ce qui permet de reconstruire une disposition enregistrée : elle ne
/// contient que des noms, et le registre dit comment fabriquer l'entité qui va
/// avec. Sans cette déclaration, une disposition relue affiche des panneaux
/// « inconnus » à la place des nôtres.
pub fn register(app: &Entity<ClaudhubApp>, cx: &mut App) {
    macro_rules! declare {
        ($($name:ident => $id:literal),* $(,)?) => { $(
            let handle = app.clone();
            register_panel(cx, $id, move |_state, _window, cx| {
                let handle = handle.clone();
                panel_handle(cx.new(|cx| $name::new(&handle, cx)))
            });
        )* };
    }
    declare! {
        SidebarPanel => "ClaudhubSidebar",
        BranchesPanel => "ClaudhubBranches",
        ChangesPanel => "ClaudhubChanges",
        BranchPanel => "ClaudhubBranch",
        HistoryPanel => "ClaudhubHistory",
        NotesPanel => "ClaudhubNotes",
        ConflictsPanel => "ClaudhubConflicts",
        FilesPanel => "ClaudhubFiles",
        DbPanel => "ClaudhubDb",
        SentryPanel => "ClaudhubSentry",
        DiffPanel => "ClaudhubDiff",
        EditorPanel => "ClaudhubEditor",
        ConsolePanel => "ClaudhubConsole",
        SettingsPanel => "ClaudhubSettings",
        TerminalPanel => "ClaudhubTerminal",
    }
}

/// "Hide this view", the only entry the dock's `…` menu deserves.
///
/// Everything else a panel can do lives in its own bar — the review's tree, the
/// diff's two columns, the explorer's collapse — and duplicating it here would
/// make two paths for one gesture. Hiding, for its part, is not about the
/// panel's content but about its place in the window: the dock is what holds
/// it, and the dock's menu is the only place the gesture is found for every one
/// of the views.
///
/// You come back through the main menu (`VIEWS`): a hidden view has no tab left,
/// so nothing left to click.
fn hide_view(app: &WeakEntity<ClaudhubApp>, name: &'static str, menu: PopupMenu) -> PopupMenu {
    let app = app.clone();
    menu.item(
        PopupMenuItem::new(tr!("action-hide-view"))
            .icon(crate::ui::icons::icon("eye-off"))
            .on_click(move |_, _window, cx| {
                let _ = app.update(cx, |this, cx| this.set_panel_visible(name, false, cx));
            }),
    )
}

/// La visibilité d'une vue au moment où son panneau est construit.
///
/// Lue dans les réglages et non dans `ClaudhubApp` : les panneaux sont bâtis
/// **pendant** `ClaudhubApp::new`, et y lire l'entité racine pendant qu'elle
/// se met à jour est ce que gpui refuse par une panique. Les deux disent la
/// même chose — l'application tient sa liste des réglages.
fn visible_at_startup(name: &str, cx: &App) -> bool {
    !Settings::global(cx).hidden_panels.iter().any(|n| n == name)
}

/// Le zoom est un **bouton**, pas une entrée de menu.
///
/// C'est la seule action que le dock met dans son menu `…` — aucun de nos
/// panneaux ne se ferme —, et un menu déroulant qui ne contient qu'une ligne
/// coûte deux clics pour ce qui en vaut un. `PanelControl::Toolbar` le sort
/// dans la barre d'onglets, à côté du titre.
///
/// Ce qu'on ne peut pas faire, et qu'il ne faut pas chercher :
/// `TabPanel::render_toolbar` de gpui-component 0.5.1 pose le bouton `…`
/// **sans condition**. Il reste donc affiché, son entrée de zoom grisée. Le
/// retirer demanderait de vendorer la bibliothèque pour un bouton.
fn zoom_in_toolbar() -> Option<PanelControl> {
    Some(PanelControl::Toolbar)
}

macro_rules! panels {
    ($($name:ident => ($id:literal, $title:literal, $render:ident, $pane:ident)),* $(,)?) => { $(
        pub struct $name {
            app: WeakEntity<ClaudhubApp>,
            focus: FocusHandle,
            /// Mise en cache pour la même raison que celle des conflits :
            /// `visible` est appelé pendant la construction de la disposition,
            /// donc au milieu de `ClaudhubApp::new`.
            visible: bool,
        }

        impl $name {
            pub const NAME: &'static str = $id;

            pub fn new(app: &Entity<ClaudhubApp>, cx: &mut Context<Self>) -> Self {
                // Sans cette observation, le panneau garderait l'image de
                // l'état au moment où il a été construit : c'est `ClaudhubApp`
                // qui change, pas lui.
                cx.observe(app, |this: &mut Self, app, cx| {
                    let visible = app.read(cx).panel_visible(Self::NAME);
                    if this.visible != visible {
                        this.visible = visible;
                        // C'est l'aire qui relit la visibilité de ses onglets :
                        // la notification du panneau seul ne la ferait pas
                        // disparaître.
                        cx.emit(PanelEvent::LayoutChanged);
                    }
                    cx.notify();
                })
                .detach();
                Self {
                    app: app.downgrade(),
                    focus: cx.focus_handle(),
                    visible: visible_at_startup(Self::NAME, cx),
                }
            }
        }

        impl Focusable for $name {
            fn focus_handle(&self, _: &App) -> FocusHandle {
                self.focus.clone()
            }
        }

        impl EventEmitter<PanelEvent> for $name {}

        // Deux traits depuis la refonte du dock : `BasePanel` porte ce qui
        // décide de la disposition — le nom persisté, la visibilité, la
        // fermeture, le zoom — et vit dans `gpui-base`, qui ne sait pas
        // dessiner. `Panel` porte la présentation, et n'existe que dans la
        // peau. C'est cette séparation qui permettrait d'écrire notre propre
        // peau sans reprendre le moteur.
        impl BasePanel for $name {
            fn panel_name(&self) -> &'static str {
                $id
            }

            /// Aucun panneau ne se ferme : rien ne permettrait de le rouvrir,
            /// et une revue sans sa liste de fichiers n'est plus une revue.
            fn closable(&self, _: &App) -> bool {
                false
            }

            fn visible(&self, _: &App) -> bool {
                self.visible
            }
        }

        impl Panel for $name {
            fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                tr!($title)
            }

            fn zoom_control(&self, _: &App) -> Option<PanelControl> {
                zoom_in_toolbar()
            }

            fn dropdown_menu(
                &mut self,
                menu: PopupMenu,
                _: &mut Window,
                _: &mut Context<Self>,
            ) -> PopupMenu {
                hide_view(&self.app, Self::NAME, menu)
            }
        }

        impl Render for $name {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let Some(app) = self.app.upgrade() else {
                    return div().into_any_element();
                };
                let content = app.update(cx, |app, cx| app.$render(window, cx).into_any_element());
                pane_root(&app, Pane::$pane, content, cx).into_any_element()
            }
        }
    )* };
}

panels! {
    SidebarPanel => ("ClaudhubSidebar", "panel-repositories", render_sidebar, Sidebar),
    BranchesPanel => ("ClaudhubBranches", "panel-branches", render_branches, Branches),
    ChangesPanel => ("ClaudhubChanges", "range-working", render_changes, Changes),
    BranchPanel => ("ClaudhubBranch", "range-branch", render_branch_review, Branch),
    NotesPanel => ("ClaudhubNotes", "panel-notes", render_notes, Notes),
    FilesPanel => ("ClaudhubFiles", "panel-files", render_files, Files),
    DbPanel => ("ClaudhubDb", "panel-databases", render_db, Db),
    SentryPanel => ("ClaudhubSentry", "panel-sentry", render_sentry, Sentry),
    // The centre of each screen. **Three panels and not one whose title
    // changes**: they belonged to the same one because they were fighting over
    // the central slot, and a tab announcing "Diff", "Editor" or "SQL"
    // depending on the last gesture was saying plainly that it carried three.
    // The screens give each of them its own place.
    DiffPanel => ("ClaudhubDiff", "panel-diff", render_diff, Diff),
    EditorPanel => ("ClaudhubEditor", "panel-editor", render_editor_panel, Editor),
    ConsolePanel => ("ClaudhubConsole", "panel-sql", render_console_panel, Console),
    SettingsPanel => ("ClaudhubSettings", "panel-settings", render_settings_panel, Settings),
}

/// The conflicts only appear when there are some.
///
/// `Panel::visible`, like the terminals: a permanently present "Conflicts" tab
/// would shift the others aside and serve one time in a hundred. It stays
/// visible while an operation is in progress, even with no conflicted file —
/// that is where what is needed to continue or abort it is found.
///
/// **Visibility is cached and not read on demand.** `visible` is called by
/// `TabPanel::active_panel`, including from `add_panel` while the layout is
/// being built — that is, **inside** `ClaudhubApp::new`. Reading the root
/// entity there would read it while it is being updated, which gpui refuses
/// with a panic. The observation set up in the constructor, on the other hand,
/// fires outside any borrow.
pub struct ConflictsPanel {
    app: WeakEntity<ClaudhubApp>,
    focus: FocusHandle,
    visible: bool,
}

impl ConflictsPanel {
    pub fn new(app: &Entity<ClaudhubApp>, cx: &mut Context<Self>) -> Self {
        cx.observe(app, |this: &mut Self, app, cx| {
            let app = app.read(cx);
            let visible = app.pending_operation().is_some() || !app.conflicted_files().is_empty();
            if this.visible != visible {
                this.visible = visible;
                // Le dock relit la visibilité de ses onglets quand la zone se
                // redessine : c'est la notification de l'aire, et non celle du
                // panneau, qui fait apparaître ou disparaître l'onglet.
                cx.emit(PanelEvent::LayoutChanged);
            }
            cx.notify();
        })
        .detach();
        Self {
            app: app.downgrade(),
            focus: cx.focus_handle(),
            // Faux au départ, et ce n'est pas un pis-aller : aucun dépôt n'est
            // encore ouvert quand la disposition se construit.
            visible: false,
        }
    }
}

impl Focusable for ConflictsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<PanelEvent> for ConflictsPanel {}

impl BasePanel for ConflictsPanel {
    fn panel_name(&self) -> &'static str {
        "ClaudhubConflicts"
    }
    fn closable(&self, _: &App) -> bool {
        false
    }
    fn visible(&self, _: &App) -> bool {
        self.visible
    }
}

impl Panel for ConflictsPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        tr!("panel-conflicts")
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        zoom_in_toolbar()
    }
}

impl Render for ConflictsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(app) = self.app.upgrade() else {
            return div().into_any_element();
        };
        let content = app.update(cx, |app, cx| {
            app.render_conflicts(window, cx).into_any_element()
        });
        pane_root(&app, Pane::Conflicts, content, cx).into_any_element()
    }
}

/// Les terminaux se masquent sans se fermer.
///
/// `Panel::visible` plutôt qu'une zone d'accueil repliable : gpui-component
/// interdit de déplacer le dernier panneau d'une zone, et les terminaux
/// seraient alors figés là où ils sont.
pub struct TerminalPanel {
    app: WeakEntity<ClaudhubApp>,
    focus: FocusHandle,
    /// Mise en cache pour la même raison que celle des conflits : `visible`
    /// est appelé pendant la construction de la disposition, donc au milieu de
    /// `ClaudhubApp::new`.
    visible: bool,
}

impl TerminalPanel {
    pub const NAME: &'static str = "ClaudhubTerminal";

    pub fn new(app: &Entity<ClaudhubApp>, cx: &mut Context<Self>) -> Self {
        cx.observe(app, |this: &mut Self, app, cx| {
            let visible = app.read(cx).terminal_visible(cx);
            if this.visible != visible {
                this.visible = visible;
                cx.emit(PanelEvent::LayoutChanged);
            }
            cx.notify();
        })
        .detach();
        Self {
            app: app.downgrade(),
            focus: cx.focus_handle(),
            visible: visible_at_startup(Self::NAME, cx),
        }
    }
}

impl Focusable for TerminalPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<PanelEvent> for TerminalPanel {}

impl BasePanel for TerminalPanel {
    fn panel_name(&self) -> &'static str {
        Self::NAME
    }
    fn closable(&self, _: &App) -> bool {
        false
    }
    fn visible(&self, _: &App) -> bool {
        self.visible
    }
}

impl Panel for TerminalPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        tr!("panel-terminal")
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        zoom_in_toolbar()
    }

    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> PopupMenu {
        hide_view(&self.app, Self::NAME, menu)
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(app) = self.app.upgrade() else {
            return div().into_any_element();
        };
        let content = app.update(cx, |app, cx| {
            app.render_terminals(window, cx).into_any_element()
        });
        pane_frame(content, cx).into_any_element()
    }
}

/// L'historique a besoin d'être chargé la première fois qu'on le regarde.
///
/// Le faire au rendu plutôt qu'à la construction est ce qui évite un `git log`
/// sur un onglet que personne n'ouvrira ; `ensure_history` ne demande qu'une
/// fois, sans quoi chaque frame relancerait la commande.
pub struct HistoryPanel {
    app: WeakEntity<ClaudhubApp>,
    focus: FocusHandle,
    visible: bool,
}

impl HistoryPanel {
    pub const NAME: &'static str = "ClaudhubHistory";

    pub fn new(app: &Entity<ClaudhubApp>, cx: &mut Context<Self>) -> Self {
        cx.observe(app, |this: &mut Self, app, cx| {
            let visible = app.read(cx).panel_visible(Self::NAME);
            if this.visible != visible {
                this.visible = visible;
                cx.emit(PanelEvent::LayoutChanged);
            }
            cx.notify();
        })
        .detach();
        Self {
            app: app.downgrade(),
            focus: cx.focus_handle(),
            visible: visible_at_startup(Self::NAME, cx),
        }
    }
}

impl Focusable for HistoryPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<PanelEvent> for HistoryPanel {}

impl BasePanel for HistoryPanel {
    fn panel_name(&self) -> &'static str {
        Self::NAME
    }
    fn closable(&self, _: &App) -> bool {
        false
    }
    fn visible(&self, _: &App) -> bool {
        self.visible
    }
}

impl Panel for HistoryPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        tr!("panel-history")
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        zoom_in_toolbar()
    }

    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> PopupMenu {
        hide_view(&self.app, Self::NAME, menu)
    }
}

impl Render for HistoryPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(app) = self.app.upgrade() else {
            return div().into_any_element();
        };
        let content = app.update(cx, |app, cx| {
            app.ensure_history(cx);
            app.render_history(window, cx).into_any_element()
        });
        pane_root(&app, Pane::History, content, cx).into_any_element()
    }
}
