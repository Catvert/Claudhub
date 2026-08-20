//! Les barres de défilement, et le lissage de la molette.
//!
//! Une liste virtualisée ne dit pas d'elle-même où l'on en est : `uniform_list`
//! ne peint que ses entrées, et rien dans la fenêtre ne distingue « il reste
//! trois lignes » de « il en reste trois mille ». Un diff de relecture d'agent
//! en fait couramment plusieurs milliers, et l'explorateur d'un projet Laravel
//! quarante mille : la barre est le seul repère de position qu'ait ce genre de
//! liste.
//!
//! Elle se pose **par-dessus** le contenu et non à côté : elle se positionne
//! en absolu, d'où le `relative` du conteneur. Lui réserver une colonne
//! rognerait la largeur utile de chaque panneau de seize pixels, et la moitié
//! d'entre eux n'ont pas de quoi défiler la plupart du temps.
//!
//! Trois détails, tous constatés à l'écran et aucun deviné :
//!
//! - **`min_h_0` et `min_w_0`.** Le conteneur est un élément flex, dont la
//!   taille minimale vaut par défaut celle de son contenu : sans eux, il prend
//!   la hauteur des huit mille lignes de l'arbre et la largeur du plus long
//!   nom de fichier. La liste, elle, reste à la bonne taille — c'est la barre
//!   qui va se peindre trois cents pixels à droite du panneau, hors de vue.
//! - **`overflow_hidden`**, pour la même raison en aval : ce qui dépasse du
//!   conteneur ne doit pas recouvrir le panneau voisin.
//! - **`scrollbar()` de gpui-component plutôt qu'un enfant `Scrollbar` nu.**
//!   L'extension enveloppe la barre d'une couche absolue calée sur les quatre
//!   bords ; posée nue, elle ne reçoit pas de bornes utilisables et ne peint
//!   rien du tout. C'est le seul point de cette liste qui ne se déduit pas de
//!   la mise en page.
//!
//! Le conteneur de la barre est aussi le bon endroit pour l'écouteur de
//! molette : il ne défile pas lui-même, donc son gestionnaire s'exécute après
//! celui de la liste — ce que `ui::motion` attend, puisqu'il reprend un saut
//! déjà appliqué. Une seule clé sert de nom à la barre et de clé au
//! mouvement, si bien qu'aucun panneau ne peut animer le décalage d'un autre.
//!
//! L'identifiant est **donné au conteneur**, et il est distinct par appel : la
//! couche que pose `scrollbar()` s'appelle toujours `scrollbar_layer`, et sans
//! parent identifié les panneaux partageraient l'état — survol, glissement —
//! d'une seule et même barre.

use gpui::{div, prelude::*, AnyElement, Context, SharedString, Stateful, Window};
use gpui_component::scroll::{ScrollableElement, ScrollbarAxis, ScrollbarHandle};

use crate::ui::app::ClaudhubApp;
use crate::ui::motion::{Axes, ScrollMotion};

/// Une barre verticale, **sans** lissage de la molette.
///
/// Pour ce qui n'est pas un panneau : la fenêtre d'aide est bâtie dans une
/// fermeture de dialogue, qui ne reçoit qu'un `App` et ne peut donc pas
/// s'abonner à la molette par `cx.listener`.
pub fn vertical<H: ScrollbarHandle + Clone>(
    id: impl Into<SharedString>,
    handle: &H,
    content: impl IntoElement,
) -> Stateful<gpui::Div> {
    wrap(
        id,
        handle,
        ScrollbarAxis::Vertical,
        content.into_any_element(),
    )
}

/// Une barre sur les deux axes, **sans** lissage de la molette.
///
/// Le seul panneau qui en veuille est le diff : ses lignes ne sont jamais
/// renvoyées à la ligne, donc il déborde aussi en largeur, et sa molette est
/// déjà prise par le zoom — il avance son mouvement lui-même.
pub fn both<H: ScrollbarHandle + Clone>(
    id: impl Into<SharedString>,
    handle: &H,
    content: impl IntoElement,
) -> Stateful<gpui::Div> {
    wrap(id, handle, ScrollbarAxis::Both, content.into_any_element())
}

fn wrap<H: ScrollbarHandle + Clone>(
    id: impl Into<SharedString>,
    handle: &H,
    axis: ScrollbarAxis,
    content: AnyElement,
) -> Stateful<gpui::Div> {
    div()
        .id(gpui::ElementId::Name(id.into()))
        .relative()
        .size_full()
        .min_h_0()
        .min_w_0()
        .overflow_hidden()
        .child(content)
        .scrollbar(handle, axis)
}

/// Ce dont on sait tirer la poignée que gpui anime réellement.
///
/// `UniformListScrollHandle` n'est pas une poignée : c'est un état de liste
/// qui en contient une, et c'est cette dernière que la molette déplace.
pub trait Scrollable: ScrollbarHandle + Clone {
    fn base(&self) -> gpui::ScrollHandle;
}

impl Scrollable for gpui::ScrollHandle {
    fn base(&self) -> gpui::ScrollHandle {
        self.clone()
    }
}

impl Scrollable for gpui::UniformListScrollHandle {
    fn base(&self) -> gpui::ScrollHandle {
        self.0.borrow().base_handle.clone()
    }
}

impl ClaudhubApp {
    /// Le lissage d'un panneau, créé à sa première molette.
    ///
    /// La clé est **l'identifiant de la barre** : une seule valeur pour les
    /// deux, et il n'y a pas moyen d'animer le mouvement d'un panneau sur le
    /// décalage d'un autre — ce qui le ferait sauter d'un bout à l'autre.
    pub(super) fn motion(&mut self, id: SharedString, axes: Axes) -> &mut ScrollMotion {
        self.motions
            .entry(id)
            .or_insert_with(|| ScrollMotion::new(axes))
    }

    /// Un contenu défilant, sa barre, et le lissage de sa molette.
    ///
    /// L'écouteur est posé sur le conteneur de la barre, qui ne défile pas
    /// lui-même : il s'exécute donc **après** celui de la liste, en phase de
    /// remontée, ce qui est exactement ce que `ScrollMotion::on_wheel`
    /// attend — il reprend un saut déjà appliqué.
    pub(super) fn scrolled<H: Scrollable>(
        &mut self,
        id: impl Into<SharedString>,
        handle: &H,
        axes: Axes,
        window: &Window,
        // Le contenu **avant** le contexte : il est bâti avec un `&mut
        // Context`, et un argument qui l'emprunte déjà en partage l'en
        // empêcherait — les arguments s'évaluent de gauche à droite.
        content: impl IntoElement,
        cx: &Context<Self>,
    ) -> Stateful<gpui::Div> {
        let id: SharedString = id.into();
        let base = handle.base();
        self.motion(id.clone(), axes).advance(&base, window);
        let axis = match axes {
            Axes::Vertical => ScrollbarAxis::Vertical,
            Axes::Both => ScrollbarAxis::Both,
        };
        wrap(id.clone(), handle, axis, content.into_any_element()).on_scroll_wheel(cx.listener(
            move |this, event: &gpui::ScrollWheelEvent, window, cx| {
                if this.motion(id.clone(), axes).on_wheel(&base, event, window) {
                    cx.notify();
                }
            },
        ))
    }
}
