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
    div, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    Render, WeakEntity, Window,
};
use gpui_component::dock::{Panel, PanelEvent};

use gpui_component::dock::{register_panel, PanelView};

use crate::tr;
use crate::ui::app::ClaudhubApp;

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
            register_panel(cx, $id, move |_, _, _, _window, cx| {
                let handle = handle.clone();
                Box::new(cx.new(|cx| $name::new(&handle, cx))) as Box<dyn PanelView>
            });
        )* };
    }
    declare! {
        SidebarPanel => "ClaudhubSidebar",
        BranchesPanel => "ClaudhubBranches",
        ChangesPanel => "ClaudhubChanges",
        BranchPanel => "ClaudhubBranch",
        HistoryPanel => "ClaudhubHistory",
        DiffPanel => "ClaudhubDiff",
        TerminalPanel => "ClaudhubTerminal",
    }
}

macro_rules! panels {
    ($($name:ident => ($id:literal, $title:literal, $render:ident)),* $(,)?) => { $(
        pub struct $name {
            app: WeakEntity<ClaudhubApp>,
            focus: FocusHandle,
        }

        impl $name {
            pub fn new(app: &Entity<ClaudhubApp>, cx: &mut Context<Self>) -> Self {
                // Sans cette observation, le panneau garderait l'image de
                // l'état au moment où il a été construit : c'est `ClaudhubApp`
                // qui change, pas lui.
                cx.observe(app, |_, _, cx| cx.notify()).detach();
                Self {
                    app: app.downgrade(),
                    focus: cx.focus_handle(),
                }
            }
        }

        impl Focusable for $name {
            fn focus_handle(&self, _: &App) -> FocusHandle {
                self.focus.clone()
            }
        }

        impl EventEmitter<PanelEvent> for $name {}

        impl Panel for $name {
            fn panel_name(&self) -> &'static str {
                $id
            }

            fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                tr!($title)
            }

            /// Aucun panneau ne se ferme : rien ne permettrait de le rouvrir,
            /// et une revue sans sa liste de fichiers n'est plus une revue.
            fn closable(&self, _: &App) -> bool {
                false
            }
        }

        impl Render for $name {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let Some(app) = self.app.upgrade() else {
                    return div().into_any_element();
                };
                app.update(cx, |app, cx| app.$render(window, cx).into_any_element())
            }
        }
    )* };
}

panels! {
    SidebarPanel => ("ClaudhubSidebar", "panel-repositories", render_sidebar),
    BranchesPanel => ("ClaudhubBranches", "panel-branches", render_branches),
    ChangesPanel => ("ClaudhubChanges", "range-working", render_changes),
    BranchPanel => ("ClaudhubBranch", "range-branch", render_branch_review),
    DiffPanel => ("ClaudhubDiff", "panel-diff", render_diff),
}

/// Les terminaux se masquent sans se fermer.
///
/// `Panel::visible` plutôt qu'une zone d'accueil repliable : gpui-component
/// interdit de déplacer le dernier panneau d'une zone, et les terminaux
/// seraient alors figés là où ils sont.
pub struct TerminalPanel {
    app: WeakEntity<ClaudhubApp>,
    focus: FocusHandle,
}

impl TerminalPanel {
    pub fn new(app: &Entity<ClaudhubApp>, cx: &mut Context<Self>) -> Self {
        cx.observe(app, |_, _, cx| cx.notify()).detach();
        Self {
            app: app.downgrade(),
            focus: cx.focus_handle(),
        }
    }
}

impl Focusable for TerminalPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<PanelEvent> for TerminalPanel {}

impl Panel for TerminalPanel {
    fn panel_name(&self) -> &'static str {
        "ClaudhubTerminal"
    }

    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        tr!("panel-terminal")
    }

    fn closable(&self, _: &App) -> bool {
        false
    }

    fn visible(&self, cx: &App) -> bool {
        self.app
            .upgrade()
            .is_some_and(|app| app.read(cx).terminal_visible(cx))
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
}

impl HistoryPanel {
    pub fn new(app: &Entity<ClaudhubApp>, cx: &mut Context<Self>) -> Self {
        cx.observe(app, |_, _, cx| cx.notify()).detach();
        Self {
            app: app.downgrade(),
            focus: cx.focus_handle(),
        }
    }
}

impl Focusable for HistoryPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<PanelEvent> for HistoryPanel {}

impl Panel for HistoryPanel {
    fn panel_name(&self) -> &'static str {
        "ClaudhubHistory"
    }

    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        tr!("panel-history")
    }

    fn closable(&self, _: &App) -> bool {
        false
    }
}

impl Render for HistoryPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(app) = self.app.upgrade() else {
            return div().into_any_element();
        };
        app.update(cx, |app, cx| {
            app.ensure_history(cx);
            app.render_history(window, cx).into_any_element()
        })
    }
}
