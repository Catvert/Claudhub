//! Construction de la disposition du dock.
//!
//! Une seule fonction, et elle existe pour contourner un défaut de
//! gpui-component 0.5.1 : `DockItem::split_with_sizes` ajoute chaque panneau
//! **deux fois** à son conteneur — deux boucles identiques dans le même corps.
//!
//! S'en passer n'était pas une option : c'est le `StackPanel` qu'elle crée qui
//! sert de parent aux panneaux, et un panneau sans parent est considéré comme
//! *verrouillé* par la bibliothèque (`TabPanel::is_locked` rend vrai quand
//! `stack_panel` est `None`). Sans conteneur, plus rien ne se glisse ni ne
//! s'accueille.

use gpui::{AppContext, Axis, Entity, Pixels, WeakEntity, Window};
use gpui_component::dock::{DockArea, DockItem, StackPanel};

/// Un `DockItem::Split` dont chaque panneau n'est ajouté qu'une fois.
pub fn split(
    axis: Axis,
    items: Vec<DockItem>,
    sizes: Vec<Option<Pixels>>,
    dock_area: &WeakEntity<DockArea>,
    window: &mut Window,
    cx: &mut gpui::App,
) -> DockItem {
    let stack: Entity<StackPanel> = cx.new(|cx| {
        let mut stack = StackPanel::new(axis, window, cx);
        for (index, item) in items.iter().enumerate() {
            let size = sizes.get(index).copied().flatten();
            stack.add_panel(item.view(), size, dock_area.clone(), window, cx);
        }
        stack
    });

    DockItem::Split {
        axis,
        size: None,
        items,
        sizes,
        view: stack,
    }
}
