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
/// come from a bounded vocabulary — the literals in this crate — so the map
/// settles after a few frames and never grows again. Nothing invalidates it: a
/// name maps to one path, for good.
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

/// A glyph that **labels a word**, at the size of that word.
///
/// Every one of them sat at `xsmall`, which is twelve pixels beside the
/// fourteen of a `text_sm` name — a picture a notch smaller than the word it
/// belongs to, which reads as a picture that has not finished loading. It is
/// the tab bar where it showed first, a tab being a glyph and a name with
/// nothing else on the line, but the mismatch was the same in every list that
/// puts an icon in front of a path.
///
/// It is **not** the size of every glyph: one inside a button is sized by the
/// button, and one standing on its own — a warning at the end of a row, the
/// dot of a state — answers to nothing but itself. This is for the ones that
/// come in front of a name, and it exists so that they answer to one thing.
pub fn glyph(name: &str) -> Icon {
    use gpui_component::Sizable as _;
    icon(name).small()
}
