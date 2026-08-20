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

/// Enveloppe le contenu d'un panneau de quoi noter qu'on vient d'y cliquer.
///
/// C'est ce qui donne une cible à `Ctrl+F`. Le clic et non le focus : le dock
/// pose le focus sur l'onglet actif de **chaque** zone, il y en a trois
/// affichées en même temps, et rien là-dedans ne dit laquelle l'utilisateur
/// regarde.
///
/// En phase de **capture**, donc avant les enfants et sans qu'aucun d'eux
/// puisse l'arrêter : une ligne de diff comme une case à cocher consomment
/// leur clic, et le panneau ne saurait jamais qu'on l'a touché.
fn pane_root(
    app: &Entity<ClaudhubApp>,
    pane: Pane,
    content: impl IntoElement,
    cx: &App,
) -> impl IntoElement {
    let app = app.clone();
    // Plus de carte ici : c'est le **cadre du groupe** qui l'est désormais —
    // le fork arrondit `TabGroupSkin::frame` et espace les splits d'une
    // gouttière, si bien que la barre d'onglets et le contenu partagent la
    // même surface, sans couture ni bordure entre eux. Redessiner une carte
    // à l'intérieur remettrait la couture qu'on vient d'enlever.
    div()
        .size_full()
        .bg(cx.theme().background)
        .capture_any_mouse_down(move |_, _window, cx| {
            app.update(cx, |app, cx| app.touch_pane(pane, cx));
        })
        .child(content)
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
        SentryPanel => "ClaudhubSentry",
        DiffPanel => "ClaudhubDiff",
        TerminalPanel => "ClaudhubTerminal",
    }
}

/// Les vues qui se masquent, dans l'ordre où le menu principal les propose.
///
/// Un nom de panneau et la clé i18n de son titre. Les noms viennent des
/// constantes des panneaux eux-mêmes : une table de littéraux se serait
/// désaccordée au premier renommage, et un nom qui ne désigne plus rien ne
/// masque plus rien — en silence.
///
/// Les conflits n'y sont pas. Leur visibilité se décide toute seule — il y a
/// une opération en cours, ou il n'y en a pas — et les masquer à la main
/// reviendrait à cacher le seul endroit d'où l'on peut la terminer.
pub const VIEWS: &[(&str, &str)] = &[
    (SidebarPanel::NAME, "panel-repositories"),
    (BranchesPanel::NAME, "panel-branches"),
    (FilesPanel::NAME, "panel-files"),
    (NotesPanel::NAME, "panel-notes"),
    (ChangesPanel::NAME, "range-working"),
    (BranchPanel::NAME, "range-branch"),
    (HistoryPanel::NAME, "panel-history"),
    (SentryPanel::NAME, "panel-sentry"),
    (DiffPanel::NAME, "panel-diff"),
    (TerminalPanel::NAME, "panel-terminal"),
];

/// « Masquer cette vue », la seule entrée que le menu `…` du dock mérite.
///
/// Tout le reste de ce qu'un panneau sait faire vit dans sa propre barre —
/// l'arbre de la revue, les deux colonnes du diff, le repli de l'explorateur —
/// et le dupliquer ici ferait deux chemins pour un même geste. Masquer, lui,
/// ne concerne pas le contenu du panneau mais sa place dans la fenêtre : c'est
/// le dock qui l'accueille, et le menu du dock est le seul endroit où le geste
/// se trouve pour chacune des vues.
///
/// On revient par le menu principal (`VIEWS`) : une vue masquée n'a plus
/// d'onglet, donc plus rien à cliquer.
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
    SentryPanel => ("ClaudhubSentry", "panel-sentry", render_sentry, Sentry),
}

/// Le panneau central : un diff, ou le fichier qu'on est en train de retoucher.
///
/// **Son titre suit son contenu.** L'éditeur intégré prend la place du diff —
/// on regarde l'un *ou* l'autre, et deux onglets à faire basculer pour un
/// geste qui vient de l'explorateur seraient un aller-retour de trop — mais un
/// onglet qui annonce « Diff » pendant qu'il montre un éditeur ment sur ce
/// qu'on a sous les yeux.
///
/// Le titre est **mis en cache**, pour la même raison que la visibilité des
/// conflits : `Panel::title` est appelé par le dock au fil du rendu de la
/// barre d'onglets, et y lire l'entité racine pendant qu'elle se met à jour
/// est ce que gpui refuse par une panique.
pub struct DiffPanel {
    app: WeakEntity<ClaudhubApp>,
    focus: FocusHandle,
    editing: bool,
    visible: bool,
}

impl DiffPanel {
    pub const NAME: &'static str = "ClaudhubDiff";

    pub fn new(app: &Entity<ClaudhubApp>, cx: &mut Context<Self>) -> Self {
        cx.observe(app, |this: &mut Self, app, cx| {
            let app = app.read(cx);
            let editing = app.is_editing();
            let visible = app.panel_visible(Self::NAME);
            if this.editing != editing || this.visible != visible {
                this.editing = editing;
                this.visible = visible;
                // C'est la barre d'onglets qui porte le titre, pas le panneau :
                // sa propre notification ne suffit pas à la faire redessiner.
                cx.emit(PanelEvent::LayoutChanged);
            }
            cx.notify();
        })
        .detach();
        Self {
            app: app.downgrade(),
            focus: cx.focus_handle(),
            editing: false,
            visible: visible_at_startup(Self::NAME, cx),
        }
    }
}

impl Focusable for DiffPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<PanelEvent> for DiffPanel {}

impl BasePanel for DiffPanel {
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

impl Panel for DiffPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        if self.editing {
            tr!("panel-editor")
        } else {
            tr!("panel-diff")
        }
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

impl Render for DiffPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(app) = self.app.upgrade() else {
            return div().into_any_element();
        };
        let content = app.update(cx, |app, cx| app.render_diff(window, cx).into_any_element());
        pane_root(&app, Pane::Diff, content, cx).into_any_element()
    }
}

/// Les conflits n'apparaissent que quand il y en a.
///
/// `Panel::visible`, comme les terminaux : un onglet « Conflits » présent en
/// permanence décalerait les autres et ne servirait qu'une fois sur cent. Il
/// reste visible tant qu'une opération est en cours, même sans fichier en
/// conflit — c'est là qu'on trouve de quoi la continuer ou l'abandonner.
///
/// **La visibilité est mise en cache et non lue à la demande.** `visible` est
/// appelé par `TabPanel::active_panel`, y compris depuis `add_panel` pendant
/// la construction de la disposition — c'est-à-dire **à l'intérieur** de
/// `ClaudhubApp::new`. Y lire l'entité racine la lirait pendant qu'elle est
/// en cours de mise à jour, ce que gpui refuse par une panique. L'observation
/// posée dans le constructeur, elle, se déclenche hors de tout emprunt.
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
        app.update(cx, |app, cx| {
            app.render_terminals(window, cx).into_any_element()
        })
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
