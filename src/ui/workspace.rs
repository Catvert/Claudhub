//! Les sous-applications.
//!
//! Claudhub fait quatre métiers qui n'ont presque rien en commun : relire un
//! diff, retoucher un fichier, interroger une base, dépouiller une erreur.
//! Tant qu'ils partageaient une seule fenêtre, chacun payait la place des
//! trois autres — huit onglets au centre dont on n'en regarde jamais que deux,
//! et un panneau central qui changeait de nature selon le dernier geste.
//!
//! Chaque écran a donc **son dock**, avec ses panneaux, ses onglets et ses
//! tailles, mémorisés séparément. On passe de l'un à l'autre par la barre du
//! bas ; régler la revue ne déplace plus rien sur l'écran des bases.
//!
//! **Deux vues sont partout** : les dépôts et les terminaux. Le premier dit
//! *où* l'on travaille — le choix vaut pour les quatre écrans —, le second est
//! ce à quoi on parle pendant qu'on regarde n'importe lequel d'entre eux. Ce
//! sont donc les deux seuls panneaux instanciés une fois par dock.
//!
//! Le panneau central, lui, **cesse d'être partagé** : le diff appartient à la
//! revue, l'éditeur à l'édition, la console SQL aux bases. C'est ce que la
//! découpe achète de plus visible — un onglet dont le titre changeait de
//! « Diff » à « Éditeur » à « SQL » selon ce qu'on venait de faire disait bien
//! qu'il portait trois choses.

use gpui::{prelude::*, px, Context, Entity, Window};
use gpui_component::{
    button::{Button, ButtonGroup, ButtonVariants as _},
    dock::{DockArea, DockLayout, DockPlacement},
};

use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::panels;

/// La hauteur d'origine des terminaux, la même sur les quatre écrans : ce
/// qu'on y lit est de la même nature partout.
const TERMINAL_HEIGHT: gpui::Pixels = px(220.);

/// La largeur d'origine de la colonne de gauche.
const SIDEBAR_WIDTH: gpui::Pixels = px(280.);

/// La largeur d'origine de la colonne qui dit quoi relire, à gauche du diff.
const REVIEW_LIST_WIDTH: gpui::Pixels = px(420.);

/// Un écran, et l'ordre dans lequel la barre les propose.
///
/// L'ordre n'est pas indifférent : c'est celui du travail. On relit, on
/// corrige ce qu'on a lu, on vérifie en base ce que le code raconte, et Sentry
/// est le point de départ des jours où l'on n'a pas choisi son sujet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Workspace {
    #[default]
    Review,
    Files,
    Db,
    Sentry,
}

impl Workspace {
    pub const ALL: [Workspace; 4] = [
        Workspace::Review,
        Workspace::Files,
        Workspace::Db,
        Workspace::Sentry,
    ];

    /// Le nom sous lequel la disposition de cet écran est enregistrée.
    ///
    /// Une clé stable et non l'indice de la variante : insérer un écran au
    /// milieu ferait sinon relire la disposition du voisin.
    pub fn key(self) -> &'static str {
        match self {
            Self::Review => "review",
            Self::Files => "files",
            Self::Db => "db",
            Self::Sentry => "sentry",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|w| w.key() == key)
    }

    /// La clé i18n du nom, celui de l'infobulle.
    pub fn label(self) -> &'static str {
        match self {
            Self::Review => "workspace-review",
            Self::Files => "workspace-files",
            Self::Db => "workspace-db",
            Self::Sentry => "workspace-sentry",
        }
    }

    /// L'icône de la barre. Elle dit ce que l'écran **contient**, pas ce qu'il
    /// s'appelle : c'est elle qu'on vise, le nom n'arrive qu'en infobulle.
    pub fn icon(self) -> &'static str {
        match self {
            Self::Review => "file-diff",
            Self::Files => "file-code",
            Self::Db => "database",
            Self::Sentry => "triangle-alert",
        }
    }

    /// L'identifiant du dock de cet écran.
    ///
    /// Distinct par écran : les quatre aires coexistent, et deux qui
    /// partageraient un identifiant partageraient l'état que gpui range
    /// dessous.
    pub fn dock_id(self) -> String {
        format!("claudhub-{}", self.key())
    }

    /// Les vues que le menu « Vues » propose de masquer sur cet écran.
    ///
    /// **Propre à l'écran, et non la liste entière.** Masquer « Console SQL »
    /// depuis la revue ne ferait rien voir changer, et une entrée qui ne fait
    /// rien se lit comme une entrée cassée. Les dépôts et les terminaux sont à
    /// la fin de chacune : ils sont partout.
    pub fn views(self) -> &'static [(&'static str, &'static str)] {
        use panels::*;
        match self {
            Self::Review => &[
                (NotesPanel::NAME, "panel-notes"),
                (ChangesPanel::NAME, "range-working"),
                (BranchPanel::NAME, "range-branch"),
                (HistoryPanel::NAME, "panel-history"),
                (DiffPanel::NAME, "panel-diff"),
                (BranchesPanel::NAME, "panel-branches"),
                (SidebarPanel::NAME, "panel-repositories"),
                (TerminalPanel::NAME, "panel-terminal"),
            ],
            Self::Files => &[
                (FilesPanel::NAME, "panel-files"),
                (EditorPanel::NAME, "panel-editor"),
                (SidebarPanel::NAME, "panel-repositories"),
                (TerminalPanel::NAME, "panel-terminal"),
            ],
            Self::Db => &[
                (DbPanel::NAME, "panel-databases"),
                (ConsolePanel::NAME, "panel-sql"),
                (SidebarPanel::NAME, "panel-repositories"),
                (TerminalPanel::NAME, "panel-terminal"),
            ],
            Self::Sentry => &[
                (SentryPanel::NAME, "panel-sentry"),
                (SidebarPanel::NAME, "panel-repositories"),
                (TerminalPanel::NAME, "panel-terminal"),
            ],
        }
    }
}

/// Les panneaux d'un écran, fabriqués pour lui.
///
/// `BasePanelView` et non `PanelView` : c'est le type que rend `panel_handle`,
/// et c'est **lui** qu'il faut. `Entity<P>` sait se convertir tout seul en
/// `Arc<dyn BasePanelView>` — et le dock le prend sans broncher, mais sans la
/// présentation qui va avec : ni onglet, ni titre, ni contenu. C'est la panne
/// silencieuse de la refonte du dock, et la seule chose que `panel_handle`
/// empêche.
type View = std::sync::Arc<dyn gpui_component::dock::BasePanelView>;

/// Le contenu d'un centre, et les terminaux dessous.
///
/// Les terminaux vivent dans le **centre** et non dans une zone d'accueil : le
/// dernier panneau d'une zone ne se déplace pas, et une zone qui n'en contient
/// qu'un est donc figée. Sous le centre, la pile en compte deux — il se
/// glisse.
fn with_terminal(
    content: DockLayout,
    terminal: View,
    height: gpui::Pixels,
    cx: &mut Context<DockArea>,
) -> DockLayout {
    DockLayout::v_split()
        .child(content, Some(height - TERMINAL_HEIGHT))
        .child(
            DockLayout::tabs().panel_view(terminal, cx),
            Some(TERMINAL_HEIGHT),
        )
}

/// Fabrique les panneaux dont un écran a besoin, et pose sa disposition
/// d'origine.
///
/// Chaque écran a **ses** instances, y compris des deux vues partagées : un
/// panneau n'appartient qu'à un dock à la fois, et un seul dock est affiché.
pub fn install_default_layout(
    workspace: Workspace,
    app: &Entity<ClaudhubApp>,
    area: &mut DockArea,
    window: &mut Window,
    cx: &mut Context<DockArea>,
) {
    use gpui_component::dock::panel_handle;
    macro_rules! panel {
        ($name:ident) => {
            panel_handle(cx.new(|cx| panels::$name::new(app, cx))) as View
        };
    }

    // La moitié **fixe** d'une division est celle du bas, jamais celle du
    // haut : l'aire du dock est plus petite que la fenêtre — barres,
    // rembourrage, gouttières — et deux tailles fixes qui somment à la hauteur
    // de la fenêtre débordent. Le bas de la colonne se faisait couper les
    // coins, et la gouttière au-dessus des terminaux était avalée.
    let height = window.viewport_size().height.max(px(600.));
    // La largeur **du centre** et non celle de la fenêtre : les tailles d'une
    // division sont réparties au prorata de leur somme, et compter la colonne
    // de gauche dedans donnerait au diff une part qu'il n'a pas demandée.
    let width = (window.viewport_size().width - SIDEBAR_WIDTH).max(px(600.));
    let third = height / 3.;

    let (left, center) = match workspace {
        // La revue : de quoi choisir quoi relire à gauche du diff, et les
        // branches sous les dépôts — on choisit un worktree *puis* on regarde
        // ses branches, et devoir passer de l'un à l'autre serait un
        // aller-retour de trop.
        Workspace::Review => {
            let left = DockLayout::v_split()
                .child(
                    DockLayout::tabs().panel_view(panel!(SidebarPanel), cx),
                    None,
                )
                .child(
                    DockLayout::tabs().panel_view(panel!(BranchesPanel), cx),
                    Some(third),
                );
            let center = with_terminal(
                DockLayout::h_split()
                    // Les façons de choisir quoi relire : ce qui reste à
                    // faire et ce qu'on a eu à dire, ce qui change
                    // maintenant, ce que la branche a écrit, ce qui est déjà
                    // committé. Des onglets et non des panneaux côte à côte —
                    // ils répondent à la même question.
                    .child(
                        DockLayout::tabs()
                            // Les notes en premier : elles disent où l'on en
                            // est, là où les suivantes disent ce qu'il y a à
                            // lire. C'est par là qu'on reprend un worktree
                            // quitté hier.
                            .panel_view(panel!(NotesPanel), cx)
                            .panel_view(panel!(ChangesPanel), cx)
                            .panel_view(panel!(BranchPanel), cx)
                            .panel_view(panel!(HistoryPanel), cx)
                            // Masqué tant qu'il n'y a rien à résoudre : un
                            // onglet permanent décalerait les autres pour
                            // servir une fois sur cent.
                            .panel_view(panel!(ConflictsPanel), cx),
                        Some(REVIEW_LIST_WIDTH),
                    )
                    .child(
                        DockLayout::tabs().panel_view(panel!(DiffPanel), cx),
                        Some(width - REVIEW_LIST_WIDTH),
                    ),
                panel!(TerminalPanel),
                height,
                cx,
            );
            (left, center)
        }
        // L'édition : l'arbre du projet sous les dépôts, l'éditeur au centre.
        // L'arbre prend les deux tiers — c'est lui qu'on parcourt, la liste
        // des worktrees tient en quatre lignes.
        Workspace::Files => {
            let left = DockLayout::v_split()
                .child(
                    DockLayout::tabs().panel_view(panel!(SidebarPanel), cx),
                    None,
                )
                .child(
                    DockLayout::tabs().panel_view(panel!(FilesPanel), cx),
                    Some(height * 0.62),
                );
            let center = with_terminal(
                DockLayout::tabs().panel_view(panel!(EditorPanel), cx),
                panel!(TerminalPanel),
                height,
                cx,
            );
            (left, center)
        }
        // Les bases : l'arbre des schémas sous les dépôts, la console au
        // centre. C'est l'explorateur de PhpStorm, et le geste est le même —
        // on déplie ce qu'on cherche, on interroge ce qu'on a trouvé.
        Workspace::Db => {
            let left = DockLayout::v_split()
                .child(
                    DockLayout::tabs().panel_view(panel!(SidebarPanel), cx),
                    None,
                )
                .child(
                    DockLayout::tabs().panel_view(panel!(DbPanel), cx),
                    Some(height * 0.62),
                );
            let center = with_terminal(
                DockLayout::tabs().panel_view(panel!(ConsolePanel), cx),
                panel!(TerminalPanel),
                height,
                cx,
            );
            (left, center)
        }
        // Sentry se suffit à lui-même : la liste des issues et la trace de
        // celle qu'on a ouverte sont deux moitiés d'un même panneau.
        Workspace::Sentry => {
            let left = DockLayout::tabs().panel_view(panel!(SidebarPanel), cx);
            let center = with_terminal(
                DockLayout::tabs().panel_view(panel!(SentryPanel), cx),
                panel!(TerminalPanel),
                height,
                cx,
            );
            (left, center)
        }
    };

    area.set_center(center, window, cx);
    area.set_dock(DockPlacement::Left, left, window, cx);
    area.set_dock_size(DockPlacement::Left, SIDEBAR_WIDTH, window, cx);
}

impl ClaudhubApp {
    /// Le choix de l'écran, à gauche de la barre d'état.
    ///
    /// **Dans la barre d'état et non dans une barre à elle.** Les deux se
    /// suivaient, hautes de trente pixels à elles deux pour porter quatre
    /// boutons et un nom de branche — deux bandeaux gris empilés sous la
    /// fenêtre, là où le dock, lui, se bat pour chaque ligne. Elles disent
    /// d'ailleurs la même chose : *où* l'on est. Le nom de la branche, l'avance
    /// sur l'amont et l'écran regardé sont trois façons de répondre, et elles
    /// se lisent d'un seul coup d'œil quand elles sont sur la même ligne.
    ///
    /// Elle est peinte par la **vue racine** et non par le panneau des dépôts :
    /// un panneau se glisse ailleurs et se masque, et la navigation ne peut pas
    /// partir avec lui.
    pub(super) fn render_workspace_nav(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.workspace;
        ButtonGroup::new("workspace-nav")
            .compact()
            .children(Workspace::ALL.map(|workspace| {
                let here = workspace == current;
                Button::new(("workspace", workspace as usize))
                    .icon(icon(workspace.icon()))
                    .tooltip(tr!(workspace.label()))
                    // **Plein contre contour**, et non l'état « sélectionné »
                    // d'un groupe entier en contour : celui-ci n'est qu'un fond
                    // légèrement plus clair, invisible sur la moitié des
                    // thèmes. C'est le même constat que pour le choix du moteur
                    // d'une connexion, et « où suis-je » est exactement la
                    // question que cette barre doit répondre sans qu'on la
                    // cherche.
                    .map(|button| {
                        if here {
                            button.primary()
                        } else {
                            button.outline()
                        }
                    })
            }))
            .on_click(cx.listener(|this, selected: &Vec<usize>, window, cx| {
                let Some(index) = selected.first() else {
                    return;
                };
                let Some(workspace) = Workspace::ALL.get(*index).copied() else {
                    return;
                };
                this.enter_workspace(workspace, window, cx);
            }))
    }
}
