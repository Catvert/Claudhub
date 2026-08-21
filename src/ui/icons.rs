//! Icons.
//!
//! The Lucide SVGs are embedded in the binary and served to gpui by the
//! `AssetSource` in `ui::mod`. gpui-component also resolves its own `IconName`
//! under `icons/`, so every built-in icon we use must exist on disk.

use gpui_component::Icon;

pub fn icon(name: &str) -> Icon {
    Icon::empty().path(format!("icons/{name}.svg"))
}
