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
    /// The settings, and the log they let you read.
    ///
    /// **A screen and not a dialog.** They were a modal window, which is what
    /// one reaches for when there is nowhere to put a form: it covered what you
    /// were adjusting, it could not be left open while you looked at the effect,
    /// and the two things one comes here for — trying a theme, reading why
    /// something failed — are exactly the two that want the rest of the window
    /// still visible. A screen costs nothing that was not already built: the
    /// bar was there, the dock knew how to carry a panel.
    Settings,
}

impl Workspace {
    pub const ALL: [Workspace; 5] = [
        Workspace::Review,
        Workspace::Files,
        Workspace::Db,
        Workspace::Sentry,
        Workspace::Settings,
    ];

    /// The name this screen's layout is saved under.
    ///
    /// A stable key and not the variant's index: inserting a screen in the
    /// middle would otherwise read back the neighbour's layout.
    pub fn key(self) -> &'static str {
        match self {
            Self::Review => "review",
            Self::Files => "files",
            Self::Db => "db",
            Self::Sentry => "sentry",
            Self::Settings => "settings",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|w| w.key() == key)
    }

    /// The i18n key of the name, the one in the tooltip.
    pub fn label(self) -> &'static str {
        match self {
            Self::Review => "workspace-review",
            Self::Files => "workspace-files",
            Self::Db => "workspace-db",
            Self::Sentry => "workspace-sentry",
            Self::Settings => "workspace-settings",
        }
    }

    /// The bar's icon. It says what the screen **contains**, not what it is
    /// called: it is what you aim at, the name only comes in the tooltip.
    pub fn icon(self) -> &'static str {
        match self {
            Self::Review => "file-diff",
            Self::Files => "file-code",
            Self::Db => "database",
            Self::Sentry => "triangle-alert",
            Self::Settings => "settings",
        }
    }

    /// This screen's dock id.
    ///
    /// Distinct per screen: the areas coexist, and two sharing an id would
    /// share the state gpui files under it.
    pub fn dock_id(self) -> String {
        format!("claudhub-{}", self.key())
    }

    /// The views the "Views" menu offers to hide on this screen.
    ///
    /// **Per screen, and not the whole list.** Hiding "SQL console" from the
    /// review would make nothing visibly change, and an entry that does nothing
    /// reads as a broken entry. The repositories and the terminals are at the
    /// end of each: they are everywhere.
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
            // The settings themselves are not offered: this screen holds them
            // and nothing else, and hiding them would leave a screen the bar
            // still points at with nothing on it.
            Self::Settings => &[(TerminalPanel::NAME, "panel-terminal")],
        }
    }
}

/// A screen's panels, made for it.
///
/// `BasePanelView` and not `PanelView`: that is the type `panel_handle`
/// returns, and it is **the** one needed. `Entity<P>` converts itself into
/// `Arc<dyn BasePanelView>` — and the dock takes it without complaint, but
/// without the presentation that goes with it: no tab, no title, no content.
/// That is the silent failure of the dock rework, and the only thing
/// `panel_handle` prevents.
type View = std::sync::Arc<dyn gpui_component::dock::BasePanelView>;

/// A centre's content, with the terminals underneath.
///
/// The terminals live in the **centre** and not in a dock zone: the last panel
/// of a zone does not move, so a zone containing only one is frozen. Under the
/// centre, the stack holds two — it can be dragged.
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

    // The **fixed** half of a split is the bottom one, never the top: the dock
    // area is smaller than the window — bars, padding, gutters — and two fixed
    // sizes adding up to the window height overflow. The bottom of the column
    // had its corners cut, and the gutter above the terminals was swallowed.
    let height = window.viewport_size().height.max(px(600.));
    // The width **of the centre** and not of the window: a split's sizes are
    // shared out in proportion to their sum, and counting the left column in
    // would give the diff a share it never asked for.
    let width = (window.viewport_size().width - SIDEBAR_WIDTH).max(px(600.));
    let third = height / 3.;

    // `Option`, because one screen has no left column: the settings do not talk
    // about a worktree, and a repository list beside them would be a picker for
    // a choice that changes nothing on the page.
    let (left, center): (Option<DockLayout>, DockLayout) = match workspace {
        // The review: what is needed to choose what to review, left of the
        // diff, and the branches under the repositories — you choose a worktree
        // *then* look at its branches, and having to switch between the two
        // would be one round trip too many.
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
            (Some(left), center)
        }
        // Editing: the project tree under the repositories, the editor in the
        // centre. The tree takes two thirds — it is what you browse, the
        // worktree list fits in four lines.
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
            (Some(left), center)
        }
        // The databases: the schema tree under the repositories, the console in
        // the centre. This is PhpStorm's explorer, and the gesture is the same —
        // you unfold what you are looking for, you query what you have found.
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
            (Some(left), center)
        }
        // Sentry stands alone: the issue list and the trace of the one opened
        // are two halves of a single panel.
        Workspace::Sentry => {
            let left = DockLayout::tabs().panel_view(panel!(SidebarPanel), cx);
            let center = with_terminal(
                DockLayout::tabs().panel_view(panel!(SentryPanel), cx),
                panel!(TerminalPanel),
                height,
                cx,
            );
            (Some(left), center)
        }
        // The settings take the whole width: the form has a sidebar of its own,
        // and two side by side would be two lists of pages to read before
        // finding the field. The terminals stay underneath — a setting is
        // adjusted then checked, and what checks it is a shell.
        Workspace::Settings => {
            let center = with_terminal(
                DockLayout::tabs().panel_view(panel!(SettingsPanel), cx),
                panel!(TerminalPanel),
                height,
                cx,
            );
            (None, center)
        }
    };

    area.set_center(center, window, cx);
    if let Some(left) = left {
        area.set_dock(DockPlacement::Left, left, window, cx);
        area.set_dock_size(DockPlacement::Left, SIDEBAR_WIDTH, window, cx);
    }
}

impl ClaudhubApp {
    /// The screen picker, at the left of the status bar.
    ///
    /// **In the status bar and not in a bar of its own.** The two followed each
    /// other, thirty pixels tall between them to carry a handful of buttons and a branch
    /// name — two grey bands stacked under the window, where the dock fights for
    /// every line. They say the same thing anyway: *where* you are. The branch
    /// name, how far ahead of the upstream you are and the screen you are
    /// looking at are three ways of answering, and they read at a single glance
    /// when they are on the same line.
    ///
    /// It is painted by the **root view** and not by the repositories panel: a
    /// panel gets dragged elsewhere and hidden, and navigation cannot leave with
    /// it.
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
