//! A list of refs read from the common `.git` — the tags, the stashes — and
//! the panel that shows it: a bar, the search's field, and the rows the search
//! keeps.
//!
//! Both panels had the same state, the same guard and the same list, written
//! twice; what differs between them is what a row paints and what it does.

use std::rc::Rc;

use gpui_kit::component::{v_flex, ActiveTheme};
use gpui_kit::{div, prelude::*, uniform_list, AnyElement, App, Context, SharedString, Window};

use crate::ui::app::ClaudhubApp;

/// What is known of one list of a repository.
pub struct Listed<T> {
    /// Behind an `Rc`: the row closure runs for every visible row on every
    /// frame and cannot read the application back, so it has to capture the
    /// list — and copying a few hundred entries to do so was a panel's whole
    /// cost.
    pub items: Rc<Vec<T>>,
    /// A read has gone out and has not come back. Without this guard, every
    /// frame would restart the command for the whole length of the read — it
    /// is the panel that asks, and it asks at render time.
    pub pending: bool,
    /// The list has come back at least once.
    ///
    /// Without it, a repository with an empty list would ask again on every
    /// frame: an empty list and a list never read are the same thing to look
    /// at and two different things to act on — the four-state `Load` of the
    /// database tree, cut down to what these panels need. Not that `Load`
    /// itself: a refresh here keeps the rows it replaces on screen, which
    /// `Load::Loading` cannot hold.
    pub loaded: bool,
}

impl<T> Default for Listed<T> {
    fn default() -> Self {
        Self {
            items: Rc::new(Vec::new()),
            pending: false,
            loaded: false,
        }
    }
}

impl<T> Listed<T> {
    /// Should a first read go out? Counts it as gone out when it should:
    /// asked at render time, a read goes out once.
    pub fn ask(&mut self) -> bool {
        if self.pending || self.loaded {
            return false;
        }
        self.pending = true;
        true
    }

    /// A read has come back.
    pub fn arrived(&mut self, items: Vec<T>) {
        self.items = Rc::new(items);
        self.pending = false;
        self.loaded = true;
    }

    /// A read goes out again. `forget` empties the list meanwhile, for a
    /// panel whose rows stop being vouched for the moment one refreshes.
    pub fn refresh(&mut self, forget: bool) {
        if forget {
            self.items = Rc::new(Vec::new());
            self.loaded = false;
        }
        self.pending = true;
    }

    /// The indices of the entries the search keeps: a frame then costs no
    /// copy of an entry.
    pub fn kept(&self, keep: impl Fn(&T) -> bool) -> Vec<usize> {
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| keep(item))
            .map(|(index, _)| index)
            .collect()
    }
}

/// What the theme gives a two-storey row, read once per frame and not per row.
#[derive(Clone, Copy)]
pub struct Look {
    pub row: gpui_kit::Pixels,
    pub muted: gpui_kit::Hsla,
    pub accent: gpui_kit::Hsla,
    pub warning: gpui_kit::Hsla,
    pub info: gpui_kit::Hsla,
}

impl Look {
    pub fn of(cx: &App) -> Self {
        Self {
            // Two storeys: what the entry is, then what it points at.
            row: crate::ui::theme::row_height(cx) * 2.,
            muted: cx.theme().muted_foreground,
            accent: cx.theme().accent,
            warning: cx.theme().warning,
            info: cx.theme().info,
        }
    }
}

/// What an empty panel says: a read under way, a search that found nothing,
/// or a repository with nothing in the list — three different things, and
/// saying the wrong one is how a panel reads as broken.
pub struct Empty {
    pub icon: &'static str,
    pub loading: SharedString,
    pub none: SharedString,
}

fn empty_list(query: &str, pending: bool, empty: Empty, cx: &App) -> AnyElement {
    let message = if pending {
        empty.loading
    } else if query.trim().is_empty() {
        empty.none
    } else {
        crate::tr!("find-no-match")
    };
    crate::ui::theme::empty(empty.icon, message, cx).into_any_element()
}

/// One panel's list, as [`ClaudhubApp::render_listed`] paints it.
pub struct Panel<'a, T> {
    /// Names the panel: its scrolled area is `<id>-bar`, its list `<id>-rows`.
    pub id: &'static str,
    pub items: Rc<Vec<T>>,
    /// The indices the search keeps — [`Listed::kept`].
    pub rows: Vec<usize>,
    pub pending: bool,
    pub query: &'a str,
    pub scroll: &'a gpui_kit::UniformListScrollHandle,
    pub empty: Empty,
}

impl ClaudhubApp {
    /// The bar, the search's field, then the rows — or what an empty list
    /// says. `row` paints the entry at `at` of the list as the row `index` of
    /// what is shown; it captures the list and that index, never a copy of
    /// the entry.
    pub(super) fn render_listed<T: 'static>(
        &mut self,
        panel: Panel<'_, T>,
        bar: impl IntoElement,
        find: Option<impl IntoElement>,
        row: impl Fn(usize, &Rc<Vec<T>>, usize, &App) -> AnyElement + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if panel.rows.is_empty() {
            return v_flex()
                .size_full()
                .child(bar)
                .children(find)
                .child(empty_list(panel.query, panel.pending, panel.empty, cx))
                .into_any_element();
        }
        let (items, rows) = (panel.items, Rc::new(panel.rows));
        let count = rows.len();
        let scroll = panel.scroll.clone();
        v_flex()
            .size_full()
            .child(bar)
            .children(find)
            .child(
                div().flex_1().min_h_0().child(
                    self.scrolled(
                        format!("{}-bar", panel.id),
                        &scroll,
                        crate::ui::motion::Axes::Vertical,
                        window,
                        uniform_list(
                            SharedString::from(format!("{}-rows", panel.id)),
                            count,
                            move |visible, _window, cx| {
                                visible
                                    .map(|index| match rows.get(index) {
                                        Some(&at) => row(index, &items, at, cx),
                                        None => div().into_any_element(),
                                    })
                                    .collect::<Vec<_>>()
                            },
                        )
                        .size_full()
                        .track_scroll(&scroll.clone()),
                        cx,
                    ),
                ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Asked at render time, a read goes out once — and once more only when
    /// refreshed.
    #[test]
    fn a_list_is_asked_once() {
        let mut list: Listed<u32> = Listed::default();
        assert!(list.ask());
        assert!(!list.ask());
        list.arrived(vec![1, 2, 3]);
        assert!(!list.ask());
        assert_eq!(list.kept(|n| n % 2 == 1), vec![0, 2]);

        // A refresh that keeps its rows keeps them on screen meanwhile…
        list.refresh(false);
        assert!(list.pending && list.loaded);
        assert_eq!(list.items.len(), 3);
        // …and one that forgets them says it has nothing yet.
        list.refresh(true);
        assert!(list.pending && !list.loaded);
        assert!(list.items.is_empty());
    }
}
