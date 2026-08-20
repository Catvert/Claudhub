//! Les barres de défilement.
//!
//! Une liste virtualisée ne dit pas d'elle-même où l'on en est : `uniform_list`
//! ne peint que ses entrées, et rien dans la fenêtre ne distingue « il reste
//! trois lignes » de « il en reste trois mille ». Un diff de relecture d'agent
//! en fait couramment plusieurs milliers, et l'explorateur d'un projet Laravel
//! quarante mille : la barre est le seul repère de position qu'ait ce genre de
//! liste.
//!
//! Elle se pose **par-dessus** le contenu et non à côté (`Scrollbar` se
//! positionne en absolu, d'où le `relative` du conteneur) : lui réserver une
//! colonne rognerait la largeur utile de chaque panneau de dix pixels, et la
//! moitié des panneaux n'ont pas de quoi défiler la plupart du temps.
//!
//! L'identifiant est **donné explicitement**. `Scrollbar::new` le déduit sinon
//! de la ligne d'appel, qui serait ici la même pour tout le monde : les
//! panneaux partageraient alors l'état — survol, glissement, minuterie
//! d'effacement — d'une seule et même barre.

use gpui::{div, prelude::*, AnyElement, Div, ElementId};
use gpui_component::scroll::{Scrollbar, ScrollbarAxis, ScrollbarHandle};

/// Enveloppe un contenu défilant de sa barre verticale.
pub fn vertical<H: ScrollbarHandle + Clone>(
    id: impl Into<ElementId>,
    handle: &H,
    content: impl IntoElement,
) -> Div {
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
) -> Div {
    wrap(id, handle, ScrollbarAxis::Both, content.into_any_element())
}

fn wrap<H: ScrollbarHandle + Clone>(
    id: impl Into<ElementId>,
    handle: &H,
    axis: ScrollbarAxis,
    content: AnyElement,
) -> Div {
    div()
        .relative()
        .size_full()
        .child(content)
        .child(Scrollbar::new(handle).id(id).axis(axis))
}
