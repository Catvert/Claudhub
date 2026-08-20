//! Les barres de défilement.
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
//! L'identifiant est **donné au conteneur**, et il est distinct par appel : la
//! couche que pose `scrollbar()` s'appelle toujours `scrollbar_layer`, et sans
//! parent identifié les panneaux partageraient l'état — survol, glissement —
//! d'une seule et même barre.

use gpui::{div, prelude::*, AnyElement, ElementId};
use gpui_component::scroll::{ScrollableElement, ScrollbarAxis, ScrollbarHandle};

/// Enveloppe un contenu défilant de sa barre verticale.
pub fn vertical<H: ScrollbarHandle + Clone>(
    id: impl Into<ElementId>,
    handle: &H,
    content: impl IntoElement,
) -> impl IntoElement {
    wrap(
        id,
        handle,
        ScrollbarAxis::Vertical,
        content.into_any_element(),
    )
}

/// La même chose avec les deux axes, pour ce qui déborde aussi en largeur —
/// le diff, dont les lignes ne sont jamais renvoyées à la ligne.
pub fn both<H: ScrollbarHandle + Clone>(
    id: impl Into<ElementId>,
    handle: &H,
    content: impl IntoElement,
) -> impl IntoElement {
    wrap(id, handle, ScrollbarAxis::Both, content.into_any_element())
}

fn wrap<H: ScrollbarHandle + Clone>(
    id: impl Into<ElementId>,
    handle: &H,
    axis: ScrollbarAxis,
    content: AnyElement,
) -> impl IntoElement {
    div()
        .id(id)
        .relative()
        .size_full()
        .min_h_0()
        .min_w_0()
        .overflow_hidden()
        .child(content)
        .scrollbar(handle, axis)
}
