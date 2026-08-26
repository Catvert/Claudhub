//! A code surface: what turns an `EditorState` into a modal editor, and the
//! wheel that goes with it.
//!
//! Two panels of this window are code in an editor — the file being touched up
//! and the SQL console — and they were built on the same foundation from the
//! start (`EditorState`, its language, its line numbers, its completions). What
//! they did **not** share was everything around it: the modal keys, the block
//! cursor, the yank flash, the folds, the smoothed wheel and the zoom all lived
//! soldered to `Editing`, the per-open-file state, so the console had none of
//! them.
//!
//! This module is that harness, taken one level up. It holds no gpui type of its
//! own beyond the decoration layers it must create; the decisions stay where
//! they were — in `ui::vim`, `ui::folds` and `ui::motion`, all pure and tested.
//!
//! **A surface is named, not owned** (`Surface`): the state lives where it
//! already lived — a file's in its `Editing`, the console's in `ClaudhubApp` —
//! and every method here takes the name and goes to fetch it. Holding the two
//! behind one entity would have meant moving the file's editor out of the tab
//! that owns it.
//!
//! What the console deliberately does **not** get: the commands that name a
//! file. `:w`, `:q` and `gd` are the editor's, and a query has no path to write
//! to, no tab to close and no definition to follow — they are dropped rather
//! than mapped onto something that looks close enough.

use std::path::PathBuf;

use gpui::{div, prelude::*, px, App, Context, Entity, Pixels, SharedString, Window};
use gpui_component::{h_flex, input::EditorState, ActiveTheme};

use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::settings::Settings;

/// Which code surface a gesture is about.
///
/// Each installs its own listener, on its own element, and the name is what the
/// listener passes along so that the harness knows whose text it is reading.
///
/// **A file is named by its path, and that is not decoration.** "The file being
/// edited" is the tab a group displays, and the dock can display two of them at
/// once — a split, two files side by side. `show_file` then runs once per
/// panel and per frame, so the active tab alternates between the two while
/// nothing is even being clicked: a wheel gesture landed on whichever had been
/// painted last, and the smoothing, filed under a single key, was advanced from
/// **both** panels' renders — one motion pushing two editors, which reads as
/// two files scrolling in lockstep. The path is what tells the panel that is
/// asking from the tab that happens to be active.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Surface {
    File(PathBuf),
    Query,
}

impl Surface {
    /// The key a file's wheel smoothing is filed under, as `ui::scroll` keys
    /// the others. One key per surface and not one for all: they scroll
    /// independently, and a shared motion hands one panel's destination to the
    /// other.
    ///
    /// Built **once**, when the file opens (`Editing::scroll_key`): it is asked
    /// for at every frame of a smoothed scroll, and formatting a path is an
    /// allocation a frame has no reason to make.
    pub(super) fn file_scroll_key(path: &std::path::Path) -> SharedString {
        format!("editor-scroll:{}", path.display()).into()
    }

    /// Whether it is a file, which four vim commands ask before naming one.
    fn is_file(&self) -> bool {
        matches!(self, Surface::File(_))
    }
}

/// The modal state of one code surface.
///
/// Created **once** with the surface it belongs to — a file when it opens, the
/// console when the window is built — for the reason every gpui entity is: the
/// decoration layers follow the text through its edits, and asking for new ones
/// per frame would stack them up for as long as the surface is alive.
pub(super) struct VimHost {
    /// The modal state proper. One per surface: leaving the console in normal
    /// mode and coming back to a file in insert mode is what a tabbed vim does
    /// anyway.
    pub vim: crate::ui::vim::Vim,
    /// The layer a yank lights up on.
    pub flash: gpui_component::input::TextDecorationCollection,
    /// What puts the light out, held so that it can be **dropped**: a second
    /// yank replaces the task, and dropping a gpui task cancels it — without
    /// that, the first timer would darken the second yank.
    pub flash_timer: Option<gpui::Task<()>>,
    /// The layer the block cursor is painted on.
    ///
    /// It is **ours** and not the editor's selection, for two reasons that are
    /// the same reason: the selection is only ever written by a keystroke, so
    /// there was no cursor at all until the first key was pressed and none again
    /// after a click of the mouse; and its colour is the theme's `selection`,
    /// which is a few percent of lightness away from the background — a block
    /// one has to look for is a block one does not see.
    pub cursor: gpui_component::input::TextDecorationCollection,
    /// The layer a blockwise selection is painted on, created **after** the
    /// cursor's so that the block cursor keeps its colour where the two meet.
    ///
    /// It exists because the editor's own selection is a single run of text and
    /// a rectangle is one range per line: there is nothing to hand it.
    pub selection: gpui_component::input::TextDecorationCollection,
    /// The layer the occurrences of a search are lit on, created **last** so
    /// that the block cursor, the yank flash and a blockwise selection all keep
    /// their colours where they cross one.
    pub matches: gpui_component::input::TextDecorationCollection,
    /// The pattern and text length the occurrences were found for.
    ///
    /// `find_all` walks the whole file, which is a keystroke's worth of work and
    /// not a frame's: it is redone when the pattern changes and when the text
    /// does, and never otherwise. **The caret is deliberately not in this key**:
    /// it moves at every `j`, and the search does not have to be run again to
    /// find out which occurrence one has landed on — that is `matches_lit`.
    pub matches_at: Option<(String, usize)>,
    /// Where the occurrences are, kept from one frame to the next so that a
    /// caret moving over them costs a comparison and not a walk of the file.
    pub matches_found: Vec<std::ops::Range<usize>>,
    /// Which of `matches_found` was painted as the current one, so that a caret
    /// that moves without changing the answer repaints nothing.
    pub matches_lit: Option<usize>,
    /// The mode, selection, head and text length the cursor was last painted
    /// for.
    ///
    /// The block is recomputed at a frame only when one of them has moved:
    /// `value()` copies the whole text, and this runs at every frame. The head
    /// is in there on its own account — `o` swaps the two ends of a visual
    /// selection without changing the range, and the block has to change ends
    /// with it.
    pub cursor_at: Option<(
        crate::ui::vim::Mode,
        std::ops::Range<usize>,
        usize,
        usize,
        bool,
    )>,
    /// Whether the next selection to arrive is to be taken as vim's own
    /// whatever it says.
    ///
    /// Undo and redo are the editor's, they are **deferred**, and they put back
    /// the selection their transaction was made from — a selection vim did not
    /// write and did not ask for, which the next frame would otherwise read as a
    /// drag of the mouse and answer with visual mode.
    pub absorb_selection: bool,
    /// The selection vim itself last wrote, read back from the editor.
    ///
    /// It is what tells a selection of ours from one made with the mouse: they
    /// arrive by the same door — `selected_range()` — and nothing announces a
    /// drag. Read back rather than remembered from the `Change`, since the
    /// editor clips what it is given to character boundaries.
    pub selection_at: Option<std::ops::Range<usize>>,
    /// The pattern and occurrence index the search bar was last centred on.
    ///
    /// It is what tells a jump from one occurrence to the next apart from a
    /// caret that merely walks over one: only the first is worth moving the
    /// page for. See `centre_search_match`.
    pub search_centred: Option<(String, usize)>,
    /// Where `zm` and `zr` have got to: the nesting level below which folds are
    /// closed. `None` is everything open, which is one past the deepest — the
    /// state `zR` puts the surface back into, and the one it opens in.
    pub fold_level: Option<usize>,
}

impl VimHost {
    /// The three decoration layers, in the order that decides who wins where
    /// they overlap.
    ///
    /// Collections are composed in creation order, the first one keeping its
    /// properties: the cursor's comes **first** so that the block stays visible
    /// through a yank's flash, which is the right way round — the flash says
    /// what was taken, the block says where one is.
    pub fn new(input: &Entity<EditorState>, cx: &mut App) -> Self {
        let (cursor, flash, selection, matches) = input.update(cx, |state, cx| {
            (
                state.create_decorations_collection(Vec::new(), cx),
                state.create_decorations_collection(Vec::new(), cx),
                state.create_decorations_collection(Vec::new(), cx),
                state.create_decorations_collection(Vec::new(), cx),
            )
        });
        Self {
            vim: crate::ui::vim::Vim::default(),
            flash,
            flash_timer: None,
            cursor,
            selection,
            matches,
            matches_at: None,
            matches_found: Vec::new(),
            matches_lit: None,
            cursor_at: None,
            absorb_selection: false,
            selection_at: None,
            search_centred: None,
            fold_level: None,
        }
    }
}

/// Where a line should sit once the view has moved.
#[derive(Clone, Copy)]
pub(super) enum Place {
    /// The least scrolling that brings it into view, and none at all when it is
    /// already there. What a motion of one line wants.
    Nearest,
    /// What `zz`, `zt` and `zb` name — and where a jump lands, which is
    /// `Centre`: arriving at a symbol on the last line of the panel shows the
    /// code that leads to it and none of what follows, and reading a definition
    /// is reading what is around it.
    Asked(crate::ui::vim::Reveal),
}

/// The line a byte offset falls on, counted from zero.
///
/// Read off the **rope**, which is what the editor holds: `value()` copies the
/// whole text to count newlines in it, and this is asked after every motion and
/// at every frame a reveal is waiting for a measurement.
pub(super) fn line_at(text: &gpui_component::input::Rope, offset: usize) -> usize {
    use gpui_component::input::RopeExt;
    text.offset_to_position(offset.min(text.len())).line as usize
}

/// How many lines `Ctrl+D` moves by half of, before the surface has been laid
/// out once and can say how tall it is.
const DEFAULT_ROWS: usize = 20;

/// How long a yank stays lit.
///
/// Not vim-highlightedyank's second, which was chosen for a terminal where
/// nothing else moves: here the mode pill, the dirty badge and an agent's
/// writing are all on the same screen, and a mark that outstays the gesture
/// reads as a state rather than as an acknowledgement.
const YANK_FLASH: std::time::Duration = std::time::Duration::from_millis(300);

impl ClaudhubApp {
    // — Reaching a surface ——————————————————————————————————————

    /// The editor a surface is showing, if it has one.
    ///
    /// The console always has one — it is built with the window — where a file
    /// only exists while a tab is open.
    pub(super) fn surface_input(&self, surface: &Surface) -> Option<Entity<EditorState>> {
        match surface {
            Surface::File(path) => self.editing_at(path).map(|editing| editing.input.clone()),
            Surface::Query => Some(self.db_query_input.clone()),
        }
    }

    /// The key a surface's wheel smoothing is filed under.
    ///
    /// Held on the `Editing` for a file, so that a smoothed scroll does not
    /// format a path at every frame; a file whose tab has just gone answers by
    /// the same rule, since the key it names has gone with it.
    fn scroll_key(&self, surface: &Surface) -> SharedString {
        match surface {
            Surface::File(path) => self
                .editing_at(path)
                .map(|editing| editing.scroll_key.clone())
                .unwrap_or_else(|| Surface::file_scroll_key(path)),
            Surface::Query => SharedString::new_static("query-scroll"),
        }
    }

    pub(super) fn surface_host(&self, surface: &Surface) -> Option<&VimHost> {
        match surface {
            Surface::File(path) => self.editing_at(path).map(|editing| &editing.host),
            Surface::Query => Some(&self.db_host),
        }
    }

    fn surface_host_mut(&mut self, surface: &Surface) -> Option<&mut VimHost> {
        match surface {
            Surface::File(path) => {
                let path = path.clone();
                self.editing_at_mut(&path).map(|editing| &mut editing.host)
            }
            Surface::Query => Some(&mut self.db_host),
        }
    }

    // — The vim keys ————————————————————————————————————————————

    /// One keystroke, when vim keys are on and a code surface has the focus.
    ///
    /// It is listened for in the **capture** phase, on an ancestor of the
    /// editor, and that placement is the whole mechanism. A key listener runs
    /// *after* the bindings have had their turn — which is what leaves `Ctrl+S`
    /// and `Alt+2` to the window — but *before* the platform hands the character
    /// to the focused input handler. Consuming the event is therefore what keeps
    /// a bare `d` from being typed into the text; letting it through is what
    /// makes insert mode an ordinary editor again.
    pub(super) fn vim_key(
        &mut self,
        surface: &Surface,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !Settings::global(cx).vim_mode || self.surface_input(surface).is_none() {
            return;
        }
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        // Alt belongs to the window — it is how one changes screen — and a
        // function key has nothing vim wants.
        if modifiers.alt || modifiers.function {
            return;
        }
        let ctrl = modifiers.control || modifiers.platform;
        // The character the keystroke **produced**, and not the key it was
        // pressed on: that is what puts `$`, `^` and `0` where they belong on an
        // AZERTY keyboard, where they are shifted or in the digit row.
        let ch = keystroke
            .key_char
            .as_deref()
            .filter(|_| !ctrl)
            .and_then(|typed| {
                let mut chars = typed.chars();
                let ch = chars.next()?;
                (chars.next().is_none() && !ch.is_control()).then_some(ch)
            });
        let key = crate::ui::vim::Key {
            ch,
            name: keystroke.key.clone(),
            ctrl,
        };
        if self.vim_press(surface, key, window, cx) {
            // Everything vim claims is ours: the key must not reach the text.
            cx.stop_propagation();
        }
    }

    /// A key the editor binds for itself, handed to vim before it gets there.
    ///
    /// `Enter` and `Backspace` are **bindings** of the input, and a binding runs
    /// **before** the capture-phase listener vim's keys go through — so neither
    /// ever reached it. That is how `Ctrl+V` is caught, and it is the same
    /// answer: the action is taken on its way down, on the same ancestor.
    ///
    /// Without it `:w` and `/foo` had no way of being run — a line one types and
    /// cannot confirm — and `Backspace` in normal mode **deleted**, which is the
    /// one keystroke of a modal editor that must not destroy anything.
    ///
    /// In insert mode both are let through, where they are what everyone means
    /// by them.
    pub(super) fn vim_named_key(
        &mut self,
        surface: &Surface,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        use crate::ui::vim::Mode;

        if !Settings::global(cx).vim_mode {
            return false;
        }
        let mode = self.surface_host(surface).map(|host| host.vim.mode());
        if !matches!(mode, Some(mode) if mode != Mode::Insert) {
            return false;
        }
        let key = crate::ui::vim::Key {
            ch: None,
            name: name.into(),
            ctrl: false,
        };
        self.vim_press(surface, key, window, cx)
    }

    /// `Ctrl+V`, which never arrives as a keystroke.
    ///
    /// The input binds it to `Paste`, and a **binding runs before** the
    /// capture-phase listener vim keys go through — so the rectangle would have
    /// been a paste, in silence. The action is therefore caught on its way down,
    /// on the same ancestor, where a capture-phase listener is ahead of the
    /// input's own. In insert mode it is let through, where it is the paste
    /// everyone means by it.
    pub(super) fn vim_paste(
        &mut self,
        surface: &Surface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        use crate::ui::vim::Mode;

        if !Settings::global(cx).vim_mode {
            return false;
        }
        let mode = self.surface_host(surface).map(|host| host.vim.mode());
        if !matches!(mode, Some(mode) if mode != Mode::Insert) {
            return false;
        }
        let key = crate::ui::vim::Key {
            ch: None,
            name: "v".into(),
            ctrl: true,
        };
        self.vim_press(surface, key, window, cx)
    }

    /// One keystroke, handed to vim — `true` when it took it.
    fn vim_press(
        &mut self,
        surface: &Surface,
        key: crate::ui::vim::Key,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        use crate::ui::vim::{Command, Response};

        let Some(input) = self.surface_input(surface) else {
            return false;
        };
        let (text, cursor, rows, folds, query) = {
            let state = input.read(cx);
            let rows = state
                .visible_row_range()
                .map(|rows| rows.len())
                .unwrap_or(DEFAULT_ROWS)
                .max(2);
            // Read afresh at every keystroke rather than remembered: the gutter
            // icons close folds too, and `zc` is not the only way in.
            let folds: Vec<crate::ui::folds::Range> = state
                .fold_candidates()
                .iter()
                .filter(|range| state.is_folded_at(range.start_line))
                .map(|range| (range.start_line, range.end_line))
                .collect();
            (
                state.value(),
                state.selected_range().start,
                rows,
                folds,
                state.search_session().query.clone(),
            )
        };
        // The clipboard is read **when a paste is about to happen**, and not at
        // every keystroke: a read is a round trip to the display server, and
        // pasting is the only gesture that consumes a register.
        let clipboard = Settings::global(cx).vim_clipboard;
        let pasting = clipboard && matches!(key.ch, Some('p') | Some('P'));
        let pasted = pasting
            .then(|| cx.read_from_clipboard().and_then(|item| item.text()))
            .flatten();
        let Some(host) = self.surface_host_mut(surface) else {
            return false;
        };
        if let Some(text) = pasted {
            host.vim.set_register(text);
        }
        host.vim.set_folds(folds);
        // The pattern `Ctrl+F` left behind, pushed in the way the folds are:
        // the editor's search bar and `/` write to the same place, and `n`
        // carrying on what was typed into the bar is the whole of what "both
        // searches, one pattern" means.
        host.vim.set_search(&query);
        let response = host.vim.press(&key, &text, cursor, rows);
        if matches!(response, Response::Ignored) {
            return false;
        }
        match response {
            Response::Ignored | Response::Consumed => {}
            Response::Apply(change) => {
                if let Some(yank) = change.yank.filter(|_| clipboard) {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(yank.text));
                }
                if !change.flash.is_empty() {
                    self.flash_yank(surface, change.flash, cx);
                }
                input.update(cx, |state, cx| {
                    if let Some(edit) = change.edit {
                        state.set_selected_range(edit.range, cx);
                        state.replace(edit.text, window, cx);
                    }
                    state.set_selected_range(change.selection, cx);
                });
                // What vim has just written, read **back**: the editor clips a
                // range to character boundaries, and this is what the next frame
                // compares against to tell our own selection from the mouse's.
                let written = input.read(cx).selected_range();
                if let Some(host) = self.surface_host_mut(surface) {
                    host.selection_at = Some(written);
                }
                self.scroll_to_line(&input, change.head, Place::Nearest, cx);
            }
            Response::Command(command) => match command {
                // Undo and redo belong to the editor, which is the only one that
                // knows what the last transaction was.
                // Both are **deferred** by gpui, and both restore the selection
                // the undone transaction was made from: the next frame would
                // find a selection vim never wrote and take it for the mouse's.
                Command::Undo => {
                    window.dispatch_action(Box::new(gpui_component::input::Undo), cx);
                    self.absorb_selection(surface);
                }
                Command::Redo => {
                    window.dispatch_action(Box::new(gpui_component::input::Redo), cx);
                    self.absorb_selection(surface);
                }
                Command::Reveal(at) => self.place_caret_line(&input, at, cx),
                Command::Scroll(lines) => self.scroll_by_lines(&input, lines, cx),
                Command::Fold(op) => self.fold(surface, op, cx),
                // The four that name a file. A query has no path to write to, no
                // tab to close and no definition to follow: on the console they
                // are dropped rather than bent into something that looks close
                // enough — `:w` running a query is a network gesture nobody
                // asked for.
                Command::Save => {
                    if surface.is_file() {
                        self.save_file(cx);
                    }
                }
                Command::Close => {
                    if surface.is_file() {
                        self.close_editor(window, cx);
                    }
                }
                Command::SaveAndClose => {
                    if surface.is_file() {
                        self.save_file(cx);
                        self.close_editor(window, cx);
                    }
                }
                Command::GoToDefinition => {
                    if surface.is_file() {
                        self.goto_definition(window, cx);
                    }
                }
            },
        }
        // And back the other way: a `/` line is what `Ctrl+F` opens on next,
        // so the two never disagree about what is being looked for.
        if let Some(pattern) = self.surface_host(surface).and_then(|host| {
            host.vim
                .highlights()
                .filter(|pattern| *pattern != query)
                .map(str::to_string)
        }) {
            // Smart case, as everywhere a pattern is read: all-lowercase
            // ignores case, a capital respects it (`ui::find`'s rule).
            let insensitive = !pattern.chars().any(char::is_uppercase);
            input.update(cx, |state, cx| {
                state.set_search_query(pattern, insensitive, cx)
            });
        }
        cx.notify();
        true
    }

    /// Says that the next selection to arrive is the editor's own doing.
    ///
    /// Undo and redo are deferred by gpui, so what they put back is read a frame
    /// later, by the very code that watches for the mouse.
    fn absorb_selection(&mut self, surface: &Surface) {
        if let Some(host) = self.surface_host_mut(surface) {
            host.absorb_selection = true;
        }
    }

    /// Paints the block cursor, and takes the editor's caret out from under it.
    ///
    /// It is **ours** and not a selection of one character, which is what it was
    /// at first. A selection is only ever written by a keystroke, so there was no
    /// cursor at all before the first key was pressed on a file that had just
    /// opened, and none again after a click of the mouse; and it is painted in
    /// the theme's `selection`, a few percent of lightness away from the
    /// background — on One Dark one had to look for it. A decoration is painted
    /// **over** the selection (the text runs come after the selection paths) and
    /// in colours of ours: the caret's, with the background showing through the
    /// glyph, which is the block cursor of every terminal.
    ///
    /// The caret is only given back where the block has nothing to cover — an
    /// empty line, the end of the text — since a cursor that disappears there
    /// would be worse than two.
    pub(super) fn sync_block_cursor(
        &mut self,
        surface: &Surface,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        let (Some(input), Some(host)) = (self.surface_input(surface), self.surface_host(surface))
        else {
            return;
        };
        let (layer, rectangle) = (host.cursor.clone(), host.selection.clone());
        let (range, caret, reversed, len) = {
            let state = input.read(cx);
            let range = state.selected_range();
            // Which end of the range the mouse was dragging. The editor keeps it
            // — `cursor()` — and it is the only thing that tells a selection
            // grown rightwards from one grown leftwards.
            let reversed = !range.is_empty() && state.cursor() == range.start;
            // The rope's length, which is borrowed: `value()` would copy the
            // whole text to learn one number.
            (range.clone(), range.start, reversed, state.text().len())
        };
        // A selection that vim did not write is one made with the mouse — or by
        // `Ctrl+A`, or by a shifted arrow the input binds for itself. The text
        // is read here rather than below so that a drag, which changes the
        // selection at every frame, copies it once and not twice.
        let mut text = None;
        if on && host.selection_at.as_ref() != Some(&range) {
            let value = input.read(cx).value();
            if let Some(host) = self.surface_host_mut(surface) {
                let was = host.vim.mode();
                if std::mem::take(&mut host.absorb_selection) {
                    // Undo's doing, not a hand's: the mode stays what it was,
                    // and normal mode reads the caret off the editor anyway.
                } else {
                    host.vim.adopt(&value, range.clone(), reversed);
                }
                host.selection_at = Some(range.clone());
                // The mode pill is read at the top of the surface's render,
                // which has already run: without a frame of its own it would say
                // "normal" over a selection until something else asked for one.
                if host.vim.mode() != was {
                    cx.notify();
                }
            }
            text = Some(value);
        }
        let Some(host) = self.surface_host(surface) else {
            return;
        };
        let (mode, head) = (host.vim.mode(), host.vim.head());
        let at = (mode, range, head, len, on);
        if host.cursor_at.as_ref() == Some(&at) {
            return;
        }
        let (block, rows) = match on {
            true => {
                let text = match text {
                    Some(text) => text,
                    None => input.read(cx).value(),
                };
                let host = match self.surface_host(surface) {
                    Some(host) => host,
                    None => return,
                };
                (
                    host.vim.cursor(&text, caret),
                    host.vim.block_selection(&text),
                )
            }
            false => (None, Vec::new()),
        };
        // An empty range means a cursor with no character under it — an empty
        // line, the end of the text.
        let wide = block.as_ref().is_some_and(|range| range.is_empty());
        let block = block.filter(|range| !range.is_empty());
        // The theme's `selection`, which is exactly what `v` and `V` look like
        // next door: this is the same gesture, on a rectangle the editor has no
        // way of holding.
        let selected = gpui::HighlightStyle {
            background_color: Some(cx.theme().selection),
            ..Default::default()
        };
        rectangle.set(
            rows.into_iter()
                .map(|range| gpui_component::input::TextDecoration::new(range, selected))
                .collect(),
            cx,
        );
        let colour = vim_mode_colour(mode, cx);
        let style = gpui::HighlightStyle {
            color: Some(ink_on(colour, cx)),
            background_color: Some(colour),
            ..Default::default()
        };
        layer.set(
            block
                .clone()
                .map(|range| gpui_component::input::TextDecoration::new(range, style))
                .into_iter()
                .collect(),
            cx,
        );
        input.update(cx, |state, cx| {
            state.set_cursor_hidden(block.is_some(), cx);
            // Where the block has nothing to cover, the caret **is** the block:
            // a bar on an empty line, blinking, says insert mode on a line that
            // is not in it. `cursor` answers an empty range there and `None`
            // only in insert mode, which is exactly the distinction wanted.
            state.set_caret_block(wide.then_some(colour), cx);
        });
        if let Some(host) = self.surface_host_mut(surface) {
            host.cursor_at = Some(at);
        }
    }

    /// Lights up every occurrence of the last search — vim's `hlsearch`.
    ///
    /// It is the half of `/` that was missing: a search that only moves the
    /// caret leaves one pressing `n` to find out whether there was anything
    /// else, and the answer is on screen all along. `Ctrl+F` lights the same
    /// occurrences from the other end, and the two share one pattern
    /// (`Vim::set_search`).
    ///
    /// Painted on a layer of ours rather than by opening the editor's search
    /// panel, and that is not a preference: opening it **takes the focus**, and
    /// the focus is what `n` and `N` need in order to stay where they are.
    ///
    /// The occurrence under the caret is lit brighter, which costs nothing —
    /// "the current one" is a position in the occurrences already found, and a
    /// caret that moves without changing it repaints nothing.
    pub(super) fn sync_search_matches(
        &mut self,
        surface: &Surface,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        let (Some(input), Some(host)) = (self.surface_input(surface), self.surface_host(surface))
        else {
            return;
        };
        let layer = host.matches.clone();
        let mut pattern = on.then(|| host.vim.highlights()).flatten().unwrap_or("");
        let (caret, len, bar) = {
            let state = input.read(cx);
            (
                state.selected_range().start,
                state.text().len(),
                state.search_session().open,
            )
        };
        // While the search bar is up, it paints its own occurrences: two layers
        // of the same colour over the same words is a denser colour, which
        // reads as a third kind of match.
        if bar {
            pattern = "";
        }
        let at = (pattern.to_string(), len);
        // The walk of the file, and only when the question has changed. What
        // invalidates it: the pattern, and the text it was run over.
        let searched = host.matches_at.as_ref() != Some(&at);
        if searched {
            let ranges = match pattern.is_empty() {
                true => Vec::new(),
                // Byte offsets, and a comparison character by character: the
                // same reckoning `Ctrl+F` makes, and the same function.
                false => crate::ui::find::find_all(pattern, &input.read(cx).value()),
            };
            if let Some(host) = self.surface_host_mut(surface) {
                host.matches_at = Some(at);
                host.matches_found = ranges;
            }
        }
        let Some(host) = self.surface_host(surface) else {
            return;
        };
        // Which occurrence the caret is in: the only thing a bare motion
        // changes, and it is a comparison over what has already been found.
        let current = host
            .matches_found
            .iter()
            .position(|range| range.contains(&caret));
        if !searched && host.matches_lit == current {
            return;
        }
        let (lit, bright) = (
            crate::ui::find::highlight_color(false, cx),
            crate::ui::find::highlight_color(true, cx),
        );
        let decorations: Vec<_> = host
            .matches_found
            .iter()
            .enumerate()
            .map(|(index, range)| {
                let style = gpui::HighlightStyle {
                    background_color: Some(match Some(index) == current {
                        true => bright,
                        false => lit,
                    }),
                    ..Default::default()
                };
                gpui_component::input::TextDecoration::new(range.clone(), style)
            })
            .collect();
        layer.set(decorations, cx);
        if let Some(host) = self.surface_host_mut(surface) {
            host.matches_lit = current;
        }
    }

    /// Centres the occurrence the editor's own search bar has just moved to.
    ///
    /// `Ctrl+F` on a code surface is the input's search, not ours, and the
    /// input scrolls **as little as it can**: the occurrence one is sent to
    /// lands on the very edge of the panel, with none of the code around it —
    /// which is what one came to read. Same ruling as a jump to a definition,
    /// and the same `Place`.
    ///
    /// Read at every frame rather than hung on a key: the bar's `Enter`, its
    /// two arrows and the query being typed all move the current match, and not
    /// one of them is ours to listen to. What it compares is the pattern and
    /// the index, so a caret walking over an occurrence moves nothing.
    pub(super) fn centre_search_match(&mut self, surface: &Surface, cx: &mut Context<Self>) {
        let Some(input) = self.surface_input(surface) else {
            return;
        };
        let (open, at, head) = {
            let state = input.read(cx);
            let session = state.search_session();
            let index = session.matcher.current_match_index();
            let head = session
                .matcher
                .matched_ranges()
                .get(index)
                .map(|range| range.start);
            (session.open, (session.query.clone(), index), head)
        };
        let Some(host) = self.surface_host_mut(surface) else {
            return;
        };
        // Closing forgets: reopening on the same pattern is a fresh search, and
        // its first occurrence wants centring like any other.
        if !open {
            host.search_centred = None;
            return;
        }
        if host.search_centred.as_ref() == Some(&at) {
            return;
        }
        host.search_centred = Some(at);
        let Some(head) = head else {
            return;
        };
        let centred = Place::Asked(crate::ui::vim::Reveal::Centre);
        // A panel that has never been laid out has nothing to divide by. The
        // memory is put back so the next frame tries again, as `reveal_at` does.
        if !self.scroll_to_line(&input, head, centred, cx) {
            if let Some(host) = self.surface_host_mut(surface) {
                host.search_centred = None;
            }
        }
    }

    /// Lights up what a yank has just copied, and puts it out again.
    ///
    /// A yank changes nothing on screen: without a sign, one is never sure it
    /// took, and one yanks again. It is what vim-highlightedyank exists for.
    ///
    /// Three things it does not do: it does not touch the selection — the block
    /// cursor is right there and two marks fighting over one character would say
    /// less than one —, it does not last a second like the plugin's default,
    /// which was chosen for a terminal where nothing else moves, and it does not
    /// hold a timer per yank: the task is **replaced**, and dropping a gpui task
    /// cancels it.
    fn flash_yank(
        &mut self,
        surface: &Surface,
        ranges: Vec<std::ops::Range<usize>>,
        cx: &mut Context<Self>,
    ) {
        let Some(host) = self.surface_host(surface) else {
            return;
        };
        let flash = host.flash.clone();
        // The tone of a search occurrence: it is already the colour this
        // interface lays over code to say "here", and it follows the theme.
        let style = gpui::HighlightStyle {
            background_color: Some(crate::ui::find::highlight_color(false, cx)),
            ..Default::default()
        };
        flash.set(
            ranges
                .into_iter()
                .map(|range| gpui_component::input::TextDecoration::new(range, style))
                .collect(),
            cx,
        );
        let timer = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(YANK_FLASH).await;
            cx.update(|cx| flash.clear(cx));
            // The editor repaints of its own accord — the collection notifies —
            // but the panel is what holds the surface, and it is what a stale
            // frame would show.
            let _ = this.update(cx, |_, cx| cx.notify());
        });
        if let Some(host) = self.surface_host_mut(surface) {
            host.flash_timer = Some(timer);
        }
    }

    // — Moving the view —————————————————————————————————————————

    /// `zz`, `zt`, `zb`: puts the caret's line where the eye wants it.
    ///
    /// Nothing here moves the caret — that is what tells these from `z.` and
    /// `z-`, which also go to the first non-blank and are therefore two answers
    /// in one, where `Response` carries a single one.
    fn place_caret_line(
        &mut self,
        input: &Entity<EditorState>,
        at: crate::ui::vim::Reveal,
        cx: &mut Context<Self>,
    ) {
        let head = input.read(cx).selected_range().start;
        self.scroll_to_line(input, head, Place::Asked(at), cx);
    }

    /// `Ctrl+E` and `Ctrl+Y`: the page moves, the caret stays.
    ///
    /// Vim drags the caret along only when the page walks out from under it,
    /// and that part is left to the editor: `set_scroll_offset` is clamped to
    /// the real range, and the caret is where it was — which is the whole point
    /// of these two keys, reading ahead without losing one's place.
    fn scroll_by_lines(
        &mut self,
        input: &Entity<EditorState>,
        lines: isize,
        cx: &mut Context<Self>,
    ) {
        input.update(cx, |state, cx| {
            let Some(line_height) = state.line_height() else {
                return;
            };
            let offset = state.scroll_offset();
            let moved = offset.y - line_height * lines as f32;
            state.set_scroll_offset(gpui::point(offset.x, moved.min(gpui::px(0.))), cx);
        });
    }

    /// The `z` commands that fold.
    ///
    /// Where a fold begins and ends is the grammar's answer, never ours: the
    /// editor holds the candidates, and all that is decided here is which of
    /// them to close. `zc`, `zo` and `za` act on the **innermost** fold holding
    /// the caret, which is the one being read.
    fn fold(&mut self, surface: &Surface, op: crate::ui::vim::Fold, cx: &mut Context<Self>) {
        use crate::ui::vim::Fold;
        let (Some(input), Some(host)) = (self.surface_input(surface), self.surface_host(surface))
        else {
            return;
        };
        let level = host.fold_level;
        let ranges: Vec<crate::ui::folds::Range> = input
            .read(cx)
            .fold_candidates()
            .iter()
            .map(|range| (range.start_line, range.end_line))
            .collect();
        if ranges.is_empty() {
            return;
        }
        let caret = {
            let state = input.read(cx);
            line_at(state.text(), state.selected_range().start)
        };
        let next = match op {
            Fold::Close | Fold::Open | Fold::Toggle => {
                let Some((start, _)) = ranges
                    .iter()
                    .filter(|(start, end)| *start <= caret && caret <= *end)
                    .max_by_key(|(start, _)| *start)
                    .copied()
                else {
                    return;
                };
                input.update(cx, |state, cx| {
                    let folded = match op {
                        Fold::Close => true,
                        Fold::Open => false,
                        _ => !state.is_folded_at(start),
                    };
                    state.set_folded(start, folded, cx);
                });
                return; // a single fold does not move the level
            }
            Fold::CloseAll => Some(0),
            Fold::OpenAll => None,
            // The ceiling is one past the deepest nesting: that is "everything
            // open", and `zr` reaching it is `zR`.
            Fold::More | Fold::Less => {
                let ceiling = crate::ui::folds::max_depth(&ranges).unwrap_or(0) + 1;
                let current = level.unwrap_or(ceiling);
                let wanted = match op {
                    Fold::More => current.saturating_sub(1),
                    _ => current + 1,
                };
                (wanted <= ceiling.saturating_sub(1)).then_some(wanted)
            }
        };
        input.update(cx, |state, cx| {
            // Rebuilt from nothing each time rather than folded on top of what
            // is there: `zr` opens a level, and reopening is not something the
            // fold map does by adding.
            state.unfold_all(cx);
            if let Some(level) = next {
                for start in crate::ui::folds::at_level(&ranges, level) {
                    state.set_folded(start, true, cx);
                }
            }
        });
        if let Some(host) = self.surface_host_mut(surface) {
            host.fold_level = next;
        }
        cx.notify();
    }

    /// Scrolls a line into view, and says whether it could.
    ///
    /// `set_selected_range` scrolls to the **end** of what it is given, which is
    /// where the caret is in normal mode but the wrong end of a selection grown
    /// upwards: `V` then `k` would walk off the top of the panel without the
    /// view ever following. A landing needs it for a plainer reason: it writes
    /// an empty range, and the editor does not always take that for a move.
    ///
    /// It answers `false` when the editor has never been laid out and there is
    /// nothing to divide by — see `Editing::reveal_at`.
    pub(super) fn scroll_to_line(
        &mut self,
        input: &Entity<EditorState>,
        head: usize,
        place: Place,
        cx: &mut Context<Self>,
    ) -> bool {
        use crate::ui::vim::Reveal;
        input.update(cx, |state, cx| {
            let (Some(rows), Some(line_height)) = (state.visible_row_range(), state.line_height())
            else {
                return false;
            };
            let row = line_at(state.text(), head);
            let span = rows.len().max(1);
            let first = match place {
                // A line already in view stays where it is: a motion that moves
                // the caret one line must not move the page under the eye.
                Place::Nearest if rows.contains(&row) => return true,
                Place::Nearest if row < rows.start => row,
                Place::Nearest => row.saturating_sub(span - 1),
                Place::Asked(Reveal::Top) => row,
                Place::Asked(Reveal::Centre) => row.saturating_sub(span / 2),
                Place::Asked(Reveal::Bottom) => row.saturating_sub(span - 1),
            };
            // The input's scroll handle counts downwards as negative.
            let offset = state.scroll_offset();
            state.set_scroll_offset(gpui::point(offset.x, -(line_height * first as f32)), cx);
            true
        })
    }

    // — The wheel ———————————————————————————————————————————————

    /// One frame of the smoothing, written back into the editor.
    ///
    /// The offset goes through `EditorState`, which is the only way in: the
    /// editor has no `ScrollHandle` of its own to hand out. Called from the
    /// surface's render, as the diff's is.
    pub(super) fn advance_surface_scroll(
        &mut self,
        surface: &Surface,
        input: &Entity<EditorState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (offset, max) = editor_extent(input, cx);
        let key = self.scroll_key(surface);
        if let Some(next) = self
            .owned_motion(key, crate::ui::motion::Axes::Vertical)
            .advance_at(offset, max, window)
        {
            input.update(cx, |state, cx| state.set_scroll_offset(next, cx));
        }
    }

    /// `Ctrl`+click on a symbol, when the editor has not followed it itself.
    ///
    /// **Bubble phase, on an ancestor of the editor, and that is the whole
    /// trick.** gpui-component answers the click itself when it has a
    /// definition at hand — one it got by `Ctrl`-hovering the symbol a moment
    /// earlier — and otherwise does what any click does: it moves the caret.
    /// Running after it therefore costs nothing and gains everything: the caret
    /// is already on the word that was clicked, so the gesture is the keyboard
    /// one from there. `followed_definition`, cleared in the capture phase of
    /// this same click, is what tells the two apart — without it, a click the
    /// editor has answered would be answered twice, the second time from the
    /// file just landed in.
    ///
    /// The surface must be the file being edited: a split shows two, and the
    /// caret this reads belongs to the active one.
    pub(super) fn on_surface_definition_click(
        &mut self,
        surface: &Surface,
        event: &gpui::MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The second down of a double click is the same gesture, and by then
        // the first has already answered it.
        if event.button != gpui::MouseButton::Left
            || !event.modifiers.secondary()
            || event.click_count > 1
        {
            return;
        }
        if std::mem::take(&mut self.followed_definition) {
            return;
        }
        let Surface::File(path) = surface else {
            return;
        };
        if !self.editing().is_some_and(|editing| &editing.path == path) {
            return;
        }
        self.goto_definition(window, cx);
    }

    /// A code surface's wheel: zoom with the platform key, smoothed scrolling
    /// otherwise — the diff's two gestures, on every panel that shows code.
    ///
    /// The same inversion as `on_diff_scroll`, and for the same reason: gpui has
    /// no capture phase for the wheel, so the editor has **already** scrolled
    /// when this runs. We give the jump back rather than try to prevent it.
    pub(super) fn on_surface_scroll(
        &mut self,
        surface: &Surface,
        event: &gpui::ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(input) = self.surface_input(surface) else {
            return;
        };
        let key = self.scroll_key(surface);
        let (offset, max) = editor_extent(&input, cx);
        // The editor's own line height and not the ambient one: it is what its
        // handler would have used, and three lines apart make a visible
        // difference on a font that is neither the interface's nor its size.
        let line = input
            .read(cx)
            .line_height()
            .unwrap_or_else(|| window.line_height())
            .max(px(1.));
        let delta = event.delta.pixel_delta(line);

        if event.modifiers.secondary() {
            // A zoom during a smoothed scroll: the destination was computed on
            // lines that no longer have the same height. Nothing to give back —
            // the event was consumed before the editor could scroll on it.
            self.owned_motion(key, crate::ui::motion::Axes::Vertical)
                .cancel();
            let steps = crate::ui::terminal_view::zoom_steps(delta.y);
            if steps != 0. {
                // The diff's size, and not one of its own: it is code on every
                // one of these surfaces, never two of them shown at the same
                // time, and a size each to keep in step would be two too many.
                crate::ui::settings::Settings::update_global(cx, |s| {
                    s.zoom(crate::ui::settings::Zoom::Diff, steps);
                });
            }
            cx.notify();
            return;
        }

        let next = match event.delta {
            // A trackpad is already gradual, and attached to the finger:
            // smoothing it would add lag to a direct gesture.
            gpui::ScrollDelta::Pixels(_) => {
                self.owned_motion(key, crate::ui::motion::Axes::Vertical)
                    .cancel();
                gpui::point(offset.x, offset.y + delta.y)
            }
            gpui::ScrollDelta::Lines(_) => self
                .owned_motion(key, crate::ui::motion::Axes::Vertical)
                .push(offset, delta, max),
        };
        input.update(cx, |state, cx| state.set_scroll_offset(next, cx));
        cx.notify();
    }

    // — The pill ————————————————————————————————————————————————

    /// The status line at the **foot** of the editor: the mode, the line being
    /// typed, and the keys of a command not finished.
    ///
    /// Where vim puts it, and the placement is the point rather than a nod to
    /// habit: a `:` line is a line one **writes**, and a line one writes at the
    /// top of a panel — above the file, next to its path and its buttons — does
    /// not read as one. It reads as a label. At the foot it is the same shape as
    /// the thing it imitates, and the eye that has just typed `:` knows where to
    /// look without being told.
    ///
    /// It is the mode's only home, and that is deliberate: it was in the file's
    /// top bar before, and two places saying the same thing is one of them too
    /// many. What still says the mode where the eye actually is, is the block
    /// cursor's colour.
    pub(super) fn render_vim_status(
        &self,
        mode: crate::ui::vim::Mode,
        prompt: Option<String>,
        pending: &str,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let mono = cx.theme().mono_font_family.clone();
        h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_2()
            .gap_2()
            .items_center()
            .border_t_1()
            .border_color(cx.theme().border)
            // The line being typed takes the mode's place rather than sitting
            // beside it, as it does in vim: while one is writing a command,
            // what one is writing is the whole of what there is to say.
            .child(match prompt {
                Some(line) => div()
                    .flex_1()
                    .truncate()
                    .text_xs()
                    .font_family(mono.clone())
                    .text_color(cx.theme().foreground)
                    .child(SharedString::from(line))
                    .into_any_element(),
                None => h_flex()
                    .flex_1()
                    .child(self.render_vim_mode(mode, "", cx))
                    .into_any_element(),
            })
            // The keys typed towards a command that is not complete: vim shows
            // them in the corner of its status line, and they are the only thing
            // that says why the next key will not do what it usually does.
            .when(!pending.is_empty(), |el| {
                el.child(
                    div()
                        .text_xs()
                        .font_family(mono)
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(pending.to_string())),
                )
            })
    }

    /// The mode pill, and what is being typed towards a command.
    ///
    /// It goes on the surface's own bar, where the eye already is, and not in
    /// the window's status bar at the other end of the screen.
    pub(super) fn render_vim_mode(
        &self,
        mode: crate::ui::vim::Mode,
        hint: &str,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colour = vim_mode_colour(mode, cx);
        h_flex()
            .gap_1()
            .items_center()
            .child(
                div()
                    .px_1p5()
                    .rounded(cx.theme().radius)
                    .text_xs()
                    .border_1()
                    .border_color(colour)
                    .text_color(colour)
                    .child(tr!(mode.key())),
            )
            .when(!hint.is_empty(), |el| {
                el.child(
                    div()
                        .text_xs()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(hint.to_string())),
                )
            })
    }
}

/// The four keys a code surface takes before its editor sees them.
///
/// One place and not two: the file editor and the SQL console install exactly
/// the same listeners, and the pair had already drifted once — a `:` line that
/// could be typed and never run on whichever of them was forgotten.
///
/// **Capture phase, on an ancestor of the editor**, and that placement is the
/// mechanism: a key listener runs after the bindings — which leaves `Ctrl+S`
/// and `Alt+2` to the window — but before the platform hands the character to
/// the input. `Ctrl+V`, `Enter` and `Backspace` never arrive as keystrokes at
/// all: the input **binds** them, and a binding runs ahead of the listener, so
/// they are caught as actions on the way down. A modified `Enter` is left
/// alone — it is somebody else's, `Ctrl+Enter` being the console's own key.
///
/// The caller installs it only when the mode is on, so that nothing stands
/// between the keyboard and the input otherwise.
pub(super) fn vim_capture(
    el: gpui::Stateful<gpui::Div>,
    surface: Surface,
    cx: &mut Context<ClaudhubApp>,
) -> gpui::Stateful<gpui::Div> {
    let (keys, paste, enter, backspace) =
        (surface.clone(), surface.clone(), surface.clone(), surface);
    el.capture_key_down(
        cx.listener(move |this, event, window, cx| this.vim_key(&keys, event, window, cx)),
    )
    .capture_action(
        cx.listener(move |this, _: &gpui_component::input::Paste, window, cx| {
            if this.vim_paste(&paste, window, cx) {
                cx.stop_propagation();
            }
        }),
    )
    .capture_action(cx.listener(
        move |this, action: &gpui_component::input::Enter, window, cx| {
            if action.secondary || action.shift {
                return;
            }
            if this.vim_named_key(&enter, "enter", window, cx) {
                cx.stop_propagation();
            }
        },
    ))
    .capture_action(cx.listener(
        move |this, _: &gpui_component::input::Backspace, window, cx| {
            if this.vim_named_key(&backspace, "backspace", window, cx) {
                cx.stop_propagation();
            }
        },
    ))
}

/// The wheel, taken before the editor sees it.
///
/// `InputState::on_scroll_wheel` scrolls and then consumes the event as soon as
/// the offset moved, so a listener on an ancestor — the diff's arrangement —
/// was never called except at the very top and bottom: nothing smoothed
/// anything, and `Ctrl`+wheel zoomed while the editor went on scrolling
/// underneath. A window mouse listener in the **capture** phase runs first, and
/// consuming the event there leaves the whole movement to us.
///
/// A `canvas` because the listener needs the laid-out bounds to know whether
/// the pointer is over this surface at all.
pub(super) fn wheel_capture(surface: Surface, cx: &mut Context<ClaudhubApp>) -> impl IntoElement {
    let entity = cx.entity();
    gpui::canvas(
        |_, _, _| (),
        move |bounds: gpui::Bounds<Pixels>, _, window, _cx| {
            window.on_mouse_event(move |event: &gpui::ScrollWheelEvent, phase, window, cx| {
                if phase != gpui::DispatchPhase::Capture || !bounds.contains(&event.position) {
                    return;
                }
                cx.stop_propagation();
                entity.update(cx, |this, cx| {
                    this.on_surface_scroll(&surface, event, window, cx)
                });
            });
        },
    )
    .absolute()
    .inset_0()
}

/// The editor's scroll offset, and how far it can go.
///
/// The travel is **read** and not worked out: it is the content the last paint
/// measured, less the viewport it was painted in — the very range
/// `set_scroll_offset` clamps to. Counting lines instead was short by the two
/// rows `visible_row_range` adds to what it shows (a `ceil` on the bottom line,
/// and one spare row), and this end of the text is clamped against on every
/// frame: the wheel stopped two lines short, and a notch given after the
/// scrollbar had reached the bottom pulled the view back up to that false
/// ceiling. A wrapped line, a folded range and the room kept under the last
/// line are all in the measurement and in none of the arithmetic.
fn editor_extent(
    input: &Entity<EditorState>,
    cx: &App,
) -> (gpui::Point<Pixels>, gpui::Point<Pixels>) {
    let state = input.read(cx);
    let offset = state.scroll_offset();
    let content = state.scroll_size().height;
    let travel = if content > px(0.) {
        (content - state.input_bounds().size.height).max(px(0.))
    } else {
        // Before the first paint there is nothing to clamp against: the motion
        // aims where it is asked to, and the editor cuts it back.
        px(f32::MAX / 4.)
    };
    (offset, gpui::point(px(0.), travel))
}

/// The colour a mode is said in — the pill on the surface's bar, and the block
/// cursor alike.
///
/// One table and not two: the word and the cursor say the same thing, and a
/// mode one reads in the corner while the block says another colour would be one
/// of them too many. It is also what makes the mode legible without looking away
/// from the caret, which is the point of a modal editor.
fn vim_mode_colour(mode: crate::ui::vim::Mode, cx: &gpui::App) -> gpui::Hsla {
    match mode {
        // The theme's own cursor colour, which is what the eye expects there.
        crate::ui::vim::Mode::Normal => cx.theme().caret,
        crate::ui::vim::Mode::Insert => cx.theme().success,
        crate::ui::vim::Mode::Visual => cx.theme().magenta,
        crate::ui::vim::Mode::VisualLine => cx.theme().cyan,
        crate::ui::vim::Mode::VisualBlock => cx.theme().yellow,
    }
}

/// The ink a block cursor's glyph takes: whichever of the theme's two extremes
/// stands out against the block.
///
/// Not `background` alone: it is the right answer on a dark theme, where the
/// block is a light colour, and the wrong one on a light theme, where a pale
/// glyph on a pale block is a hole.
fn ink_on(colour: gpui::Hsla, cx: &gpui::App) -> gpui::Hsla {
    let (background, foreground) = (cx.theme().background, cx.theme().foreground);
    let (darker, lighter) = if background.l < foreground.l {
        (background, foreground)
    } else {
        (foreground, background)
    };
    if colour.l > 0.5 {
        darker
    } else {
        lighter
    }
}
