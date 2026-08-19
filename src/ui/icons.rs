//! Icônes.
//!
//! Les SVG Lucide sont embarqués dans le binaire et servis à gpui par
//! l'`AssetSource` de `ui::mod`. gpui-component résout aussi ses propres
//! `IconName` sous `icons/`, donc toute icône intégrée qu'on utilise doit
//! exister sur disque.

use gpui_component::Icon;

pub fn icon(name: &str) -> Icon {
    Icon::empty().path(format!("icons/{name}.svg"))
}
