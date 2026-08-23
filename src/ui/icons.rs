//! Icons.
//!
//! The Lucide SVGs are embedded in the binary and served to gpui by the
//! `AssetSource` in `ui::mod`. gpui-component also resolves its own `IconName`
//! under `icons/`, so every built-in icon we use must exist on disk.

use gpui_component::Icon;

/// The path an icon's name resolves to, interned.
///
/// Two hundred call sites, every one of them in a render closure: building the
/// path meant a `String` and then an `Arc<str>` per icon per frame. The names
/// come from a bounded vocabulary — the literals in this crate plus what a
/// plugin's manifest carries — so the map settles after a few frames and never
/// grows again. Nothing invalidates it: a name maps to one path, for good.
pub fn icon(name: &str) -> Icon {
    thread_local! {
        static PATHS: std::cell::RefCell<std::collections::HashMap<Box<str>, gpui::SharedString>> =
            std::cell::RefCell::new(std::collections::HashMap::new());
    }
    let path = PATHS.with(|paths| {
        let mut paths = paths.borrow_mut();
        if let Some(path) = paths.get(name) {
            return path.clone();
        }
        let path = gpui::SharedString::from(format!("icons/{name}.svg"));
        paths.insert(Box::from(name), path.clone());
        path
    });
    Icon::empty().path(path)
}
