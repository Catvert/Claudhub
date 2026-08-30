//! The quick search: one modal, two questions.
//!
//! PhpStorm's `Ctrl+Shift+F` and its `Shift Shift`, which are the same window
//! with two tabs — *where is this text* and *which file is this*. Both are
//! asked from wherever the hand happens to be, and both are answered without
//! moving anything: a modal costs no panel, no tab and no layout, and it goes
//! away by itself once it has been answered. That is the whole reason it is a
//! dialog and not a screen — one comes here to leave.
//!
//! **It stands in front of the search panels rather than replacing them.**
//! What a modal cannot do is be read for a while: forty hits over eight files,
//! with the file itself beside the list, is a thing one scrolls and comes back
//! to, and a window that shuts on the first Escape is the wrong shape for it.
//! So `Ctrl+Entrée` hands the very same results to `ui::search_view` and steps
//! out of the way.
//!
//! **And it is the same results, not a second search.** The text side owns no
//! state at all: it types into `search_input`, sends through `run_search`, and
//! paints `search.rows` with the panel's own row renderer. There is one query
//! and one answer in this window — the escape hatch above is free because of
//! it, and a modal holding its own copy would have had to explain which of the
//! two the panel was showing.
//!
//! The file side is the other half, and it is not `ui::find`'s filter over the
//! tree: it is a **ranking**, `ui::quick`, pure and tested in front of this.

use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    div, prelude::*, px, uniform_list, App, Context, Entity, FocusHandle, SharedString, StyledText,
    Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::Input,
    v_flex, ActiveTheme, Selectable as _, Sizable as _, WindowExt as _,
};

use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::quick;
use crate::ui::search_view::PreviewPane;

/// The wheel-smoothing and scrollbar key of the list.
///
/// Its own, and not the search panel's: the two lists can be on screen at the
/// same time — the palette opens over a panel that was already showing results
/// — and one scroll handle driving two `uniform_list`s in a frame is measured
/// by whichever painted last.
const SCROLL: &str = "quick-results";
/// The same, for the file shown beside them.
const PREVIEW_SCROLL: &str = "quick-preview";

/// What the list takes of the palette's width; the file takes the rest.
///
/// A fixed column and not a share: what the list shows is a path, whose length
/// nobody chose, and what the file shows is code, which wants every pixel
/// left. Wide enough for `src/ui/search_view.rs` and a folder or two after it.
const LIST_WIDTH: gpui::Pixels = px(360.);

/// How long the arrows have to stop before the file under them is read.
///
/// Short enough not to be waited for, long enough that a key held down through
/// a ranked list reads nothing until it is let go.
const PREVIEW_DELAY: std::time::Duration = std::time::Duration::from_millis(120);

/// How large the palette gets, at most.
///
/// A **definite** box either way, never one sized by its content: a list that
/// grows as one types moves the row under the pointer between two keystrokes,
/// and the preview beside it would jump with it.
const MAX_WIDTH: gpui::Pixels = px(1280.);
const MAX_HEIGHT: gpui::Pixels = px(760.);

/// Which of the two questions the palette is asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Mode {
    /// A file, by name. `Shift Shift`, and `Ctrl+P`.
    #[default]
    Files,
    /// A word, anywhere in the checkout. `Ctrl+Shift+F`.
    Text,
}

/// What the palette holds between two openings.
///
/// Almost nothing, and that is the point: the text side reads `SearchState`,
/// and the file side keeps only what a ranking produced. The field itself is
/// `ClaudhubApp::quick_input`, beside `search_input` and for its reason — an
/// `InputState` created at render time loses the caret and the text on the
/// first keystroke.
#[derive(Default)]
pub(super) struct QuickState {
    /// The dialog is up. Read to tell a second `Shift Shift` from a first, and
    /// to keep the palette's keys from acting on a window it has left.
    pub open: bool,
    pub mode: Mode,
    /// The row the arrows are on. An index into `hits` in file mode; the text
    /// side has `search.selected`, which the panel shares.
    pub selected: usize,
    /// What the ranking answered, and the list it indexes into.
    ///
    /// The paths are the explorer's own `Rc`, never a copy: a checkout with a
    /// `target/` is a hundred thousand of them, and cloning that on a
    /// keystroke is three milliseconds one can feel.
    pub hits: Rc<Vec<quick::Hit>>,
    pub paths: Rc<Vec<PathBuf>>,
    pub scroll: gpui::UniformListScrollHandle,
    /// The file shown beside the list, on the file side.
    ///
    /// **Its own and not `search.preview`**, though it is the same type and
    /// the same pipeline fills it. What the search screen previews is the hit
    /// one is on; what this previews is a file asked for by name, and they are
    /// two answers to two questions — sharing the slot would have had the
    /// palette wipe, on every arrow, the file the panel behind it was showing.
    pub preview: Option<crate::ui::search_view::Preview>,
    pub preview_scroll: gpui::UniformListScrollHandle,
    /// Bumped on every move of the cursor, read again when the timer fires.
    ///
    /// **The file side previews on a trailing delay, the text side does not**,
    /// and the difference is in the lists. Two neighbouring hits are usually
    /// in the same file, so walking a result list costs one read per *file*
    /// crossed (`sync_search_preview` skips what is already shown); two
    /// neighbouring rows of a ranked file list are never the same file, so
    /// holding the arrow down would be one read per row. The counter is the
    /// whole of the delay, as in `search_typed`: a deferred read that finds it
    /// has moved gives up.
    pub preview_at: u64,
    /// Where `Shift Shift` stands. On the application and not on a view: the
    /// modifier changes it reads arrive at the **root**, whatever holds the
    /// keyboard.
    pub tap: quick::DoubleTap,
    /// The last query typed, **per worktree**.
    ///
    /// One field for the two questions, but not one question for the two
    /// checkouts: the palette offers back what it was last asked, and what one
    /// was looking for in another worktree is an offer that has nothing to do
    /// with this one — the class names differ, and the file that answered is
    /// not even there. In memory and not in the store: this is the question of
    /// the moment, and it is worth nothing tomorrow morning, where the recent
    /// files are.
    pub queries: std::collections::HashMap<PathBuf, String>,
}

/// The palette's content, as an entity of its own.
///
/// **An entity and not a closure**, the arrangement `settings_view::
/// SettingsForm` exists for: `open_dialog` keeps a `Fn` called back from the
/// root view's render, and reading the root entity from there is the panic
/// gpui refuses. A child's `render` happens after the parent's closure has
/// returned.
pub(super) struct QuickPalette {
    app: gpui::WeakEntity<ClaudhubApp>,
    focus: FocusHandle,
}

impl QuickPalette {
    pub(super) fn new(app: &Entity<ClaudhubApp>, cx: &mut Context<Self>) -> Self {
        // The palette is a picture of a search that answers while it is open.
        cx.observe(app, |_, _, cx| cx.notify()).detach();
        Self {
            app: app.downgrade(),
            focus: cx.focus_handle(),
        }
    }
}

impl gpui::Focusable for QuickPalette {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for QuickPalette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(app) = self.app.upgrade() else {
            return div().into_any_element();
        };
        app.update(cx, |app, cx| {
            app.render_quick(window, cx).into_any_element()
        })
    }
}

impl ClaudhubApp {
    /// Opens the palette on one of its two questions, or turns an open one to
    /// it.
    ///
    /// **A selection made in a code surface comes along**, in both modes and
    /// for one reason: what is under the caret is what one is about to look
    /// for. `Ctrl+Shift+F` on a call asks where else it is made; `Shift Shift`
    /// on a class name asks for the file that declares it, which in every
    /// language this window opens is that name with an extension. Read
    /// **before** anything is focused: the focus is what says which surface
    /// the selection is in.
    pub(super) fn open_quick(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        // **Already up, and the same two shortcuts are then the two tabs.**
        // What one is after in the middle of typing a name is the other
        // question *on those letters*, so the field is kept exactly as it is —
        // and the seed below is precisely what would take it away: nothing in
        // the palette is a code surface, so `search_seed` falls back on the
        // document the centre shows and would answer with a selection made
        // somewhere else, five minutes ago.
        if self.quick.open {
            self.switch_quick(mode, window, cx);
            return;
        }
        // The selection first, and the worktree's own last question failing
        // that: **the field is always written**, where it used to be left as
        // it was found. What it was found holding is the question asked in
        // whatever checkout one was in before, and offering the name of a
        // class that does not exist here is worse than offering nothing.
        let text = self.search_seed(window, cx).unwrap_or_else(|| {
            self.active
                .as_ref()
                .and_then(|worktree| self.quick.queries.get(worktree))
                .cloned()
                .unwrap_or_default()
        });
        // The list is git's, and it is read once per checkout: asking for it
        // here is what makes the first `Shift Shift` of a session answer.
        self.ensure_project_files(cx);
        self.quick.mode = mode;
        self.quick.open = true;
        self.quick_input.update(cx, |state, cx| {
            state.set_placeholder(mode.placeholder(), window, cx);
            state.set_value(text, window, cx);
            // **Selected whole, with or without a seed**, as the search
            // screen's field is: what is in there is the previous question —
            // the one just seeded, or the one asked a minute ago — and it is
            // an offer either way. The next keystroke replaces it; Entrée
            // asks it again.
            state.select_all(window, cx);
        });
        // `set_value` emits no change event, so nothing has been recomputed
        // behind our back: this is the only pass over the query.
        self.quick_query(window, cx);
        {
            let palette = self.quick_palette.clone();
            let app = cx.entity().downgrade();
            window.open_dialog(cx, move |dialog, window, _cx| {
                // **Read off the window rather than fixed**, as the settings
                // form is: this is two columns of which one is code, and code
                // in four hundred pixels is read one word per line. Recomputed
                // on every frame the dialog is built, so it follows a resize.
                let viewport = window.viewport_size();
                let width = viewport.width.min(MAX_WIDTH) - px(64.);
                let height = viewport.height.min(MAX_HEIGHT) - px(120.);
                dialog
                    .title(tr!("quick-title"))
                    .w(width)
                    .max_w(width)
                    // Read rather than answered: there is no OK here, the row
                    // one lands on is the answer. The overlay dismisses and
                    // the cross is there, as in the settings.
                    .overlay_closable(true)
                    .close_button(true)
                    // The palette's own keys live on this node's context, so
                    // the box has to be a **definite** one: its root is a
                    // `size_full`, which resolves against nothing when its
                    // parent is sized by what it contains.
                    .child(div().w_full().h(height).child(palette.clone()))
                    // **Entrée never answers the dialog.** It answers the
                    // row one is on, through the palette's own binding, which
                    // sits deeper in the context stack and therefore fires
                    // first. This is the belt to that brace: were the
                    // dialog's `Confirm` to win the key anyway, the worst it
                    // could do is nothing, rather than shutting the palette on
                    // the keystroke meant to use it.
                    .on_ok(|_, _, _| false)
                    // Escape, the cross and the overlay all end here, and the
                    // flag has to follow them: a palette believed open would
                    // swallow the next `Shift Shift`.
                    .on_close({
                        let app = app.clone();
                        move |_, window, cx| {
                            app.update(cx, |app, cx| app.quick_closed(window, cx)).ok();
                        }
                    })
            });
        }
        // Deferred, and by the field: a context menu hands the focus back to
        // whatever had it as it closes, *after* the handler that opened this.
        // See `dialogs::focus_field`.
        crate::ui::dialogs::focus_field(&self.quick_input, window, cx);
        cx.notify();
    }

    /// Closes the palette, whichever gesture asked.
    pub(super) fn close_quick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.quick.open {
            return;
        }
        window.close_dialog(cx);
        // Called by hand, and it has to be: `Root::close_dialog` pops the
        // dialog off its stack without running the callbacks the `Dialog`
        // itself carries, so nothing else would lower the flag or hand the
        // keyboard on from here. The other ways out reach `quick_closed`
        // through `on_close`, and it does not mind being told twice.
        self.quick_closed(window, cx);
    }

    /// The palette has gone, whichever of the four ways took it: Escape, the
    /// cross, the overlay, or a press outside it.
    ///
    /// **It is here and not in `close_quick`** because three of those four
    /// never call `close_quick` at all — they are the dialog's own gestures,
    /// and `on_close` is where they meet.
    ///
    /// What it does beyond lowering the flag is give the window a keyboard
    /// back. The palette's field dies with the dialog, and `close_dialog`
    /// hands the focus back to whatever held it before — but only when there
    /// was something: open the palette with `Maj Maj` while nothing is focused
    /// and there is nothing to hand back to, so the caret stays on a field
    /// that is no longer painted. Every key event is then walked from the
    /// dispatch tree's root, which this view's node is not, so nothing bound
    /// on it fires: neither `Ctrl+P` nor `Ctrl+Maj+F`, nor the modifier
    /// changes `Maj Maj` is read from. It came back on the next click, which
    /// is what gave the window a live focus again.
    ///
    /// The test is the field itself, and it is asked **after** the dialog has
    /// gone: whatever `close_dialog` had to hand back, it has handed back by
    /// then, so a caret still sitting on the palette's field is a caret with
    /// nowhere to go. `contains` is no help — a dialog is painted inside this
    /// very view (`Root::render_dialog_layer`, at the end of its render), so
    /// the dying field counts as one of its descendants right up to the frame
    /// that stops drawing it.
    pub(super) fn quick_closed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.quick.open = false;
        let stranded = gpui::Focusable::focus_handle(&self.quick_input, cx).is_focused(window)
            || window.focused(cx).is_none();
        if stranded {
            let root = self.focus.clone();
            root.focus(window, cx);
        }
        cx.notify();
    }

    /// The other question, on the same letters. What the two tabs do, and what
    /// the two shortcuts do on a palette that is already up.
    ///
    /// **The field is never touched here**, only its placeholder: there is one
    /// field for the two questions, so the query survives the tab by doing
    /// nothing at all, and `quick_query` asks the new question with it. The
    /// text side then types into `search_input` on its way through, which is
    /// what keeps the panel behind showing what the palette shows.
    pub(super) fn switch_quick(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        if self.quick.mode == mode {
            // The tab one is on, asked for again: nothing to switch, and the
            // caret goes back to the field — the gesture came from a shortcut
            // as often as from the tab, and a click on the tab takes the
            // keyboard with it.
            crate::ui::dialogs::focus_field(&self.quick_input, window, cx);
            return;
        }
        self.quick.mode = mode;
        self.quick_input.update(cx, |state, cx| {
            state.set_placeholder(mode.placeholder(), window, cx)
        });
        self.quick_query(window, cx);
        crate::ui::dialogs::focus_field(&self.quick_input, window, cx);
        cx.notify();
    }

    /// A keystroke in the field, and what each mode makes of it.
    ///
    /// **The file side answers now, the text side is debounced.** Ranking a
    /// hundred thousand paths is arithmetic on a list already in memory —
    /// milliseconds, and no worker — where a project-wide search is a `git
    /// grep` whose answer replaces the list one is reading. The second is
    /// `search_typed`'s trailing debounce, untouched; the first would only be
    /// made worse by waiting.
    pub(super) fn quick_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.quick_input.read(cx).value().to_string();
        // Kept under the checkout one is in, so that the next opening offers
        // this question back and not another worktree's.
        if let Some(worktree) = self.active.clone() {
            self.quick.queries.insert(worktree, text.clone());
        }
        match self.quick.mode {
            Mode::Files => self.rank_quick_files(&text, cx),
            Mode::Text => {
                // The palette types **into the search screen's field**, which
                // is what makes `Ctrl+Entrée` free: the panel behind is
                // already showing what the palette shows. `set_value` emits no
                // change event, so the debounce below is the only one.
                self.search_input
                    .update(cx, |state, cx| state.set_value(text, window, cx));
                self.search_typed(cx);
                // The pane beside the list, for the results **already** there:
                // asking the same question again sends nothing, so without
                // this the palette opened on a file-shaped hole until the
                // first arrow. It is read nowhere else now — the panel behind
                // opens its hits in the editor — see `sync_search_preview`.
                self.sync_search_preview(window, cx);
            }
        }
        cx.notify();
    }

    /// The ranking, and the cursor put back at the top.
    ///
    /// **The paths are borrowed, not copied.** `quick::rank` is given the
    /// explorer's own list as `&str`, and the answer's indices are mapped back
    /// through `origins` — a path whose bytes are not UTF-8 is left out of the
    /// candidates, and an index into a shorter list would name another file.
    fn rank_quick_files(&mut self, query: &str, cx: &mut Context<Self>) {
        let paths = self
            .explorer()
            .map(|explorer| explorer.files.clone())
            .unwrap_or_default();
        // **Nothing typed is a question too**, and its answer is the files
        // read lately in this checkout. An empty ranking matches nothing on
        // purpose — the first two hundred paths of a project are whatever
        // sorts first — so the list stood empty exactly when the palette is at
        // its most useful: opened, and one is about to go back to where one
        // was. It is its own list of paths and not a ranking over the
        // project's, so no letter is highlighted, and none should be.
        if query.is_empty() {
            let recent = self.recent_files(&paths, cx);
            let hits = (0..recent.len())
                .map(|index| quick::Hit {
                    index,
                    score: 0,
                    ranges: Vec::new(),
                })
                .collect();
            self.quick.paths = Rc::new(recent);
            self.quick.hits = Rc::new(hits);
            self.quick.selected = 0;
            self.quick
                .scroll
                .scroll_to_item(0, gpui::ScrollStrategy::Top);
            self.preview_quick_file(cx);
            cx.notify();
            return;
        }
        let mut candidates = Vec::with_capacity(paths.len());
        let mut origins = Vec::with_capacity(paths.len());
        for (index, path) in paths.iter().enumerate() {
            if let Some(text) = path.to_str() {
                candidates.push(text);
                origins.push(index);
            }
        }
        let mut hits = quick::rank(query, &candidates);
        for hit in &mut hits {
            hit.index = origins[hit.index];
        }
        self.quick.paths = paths;
        self.quick.hits = Rc::new(hits);
        // A fresh ranking is a fresh list: an index kept from the previous one
        // would point at whatever now sits in that row.
        self.quick.selected = 0;
        self.quick
            .scroll
            .scroll_to_item(0, gpui::ScrollStrategy::Top);
        self.preview_quick_file(cx);
        cx.notify();
    }

    /// The files this checkout has read lately, most recent first, as the
    /// store holds them and cut down to what the project still has.
    fn recent_files(&self, paths: &[PathBuf], cx: &App) -> Vec<PathBuf> {
        let Some(worktree) = self.active.as_deref() else {
            return Vec::new();
        };
        crate::ui::store::Store::global(cx)
            .worktree(worktree)
            .map(|saved| quick::recent(&saved.recent, paths))
            .unwrap_or_default()
    }

    /// The file under the cursor, once the arrows have stopped.
    ///
    /// Bumps the counter and reads it again at the deadline: anything the
    /// walking has overtaken gives up, so holding the arrow down through two
    /// hundred rows costs one read and not two hundred. `search_typed`'s
    /// pattern, for `search_typed`'s reason.
    fn preview_quick_file(&mut self, cx: &mut Context<Self>) {
        self.quick.preview_at = self.quick.preview_at.wrapping_add(1);
        let at = self.quick.preview_at;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PREVIEW_DELAY).await;
            let _ = this.update(cx, |this, cx| {
                if this.quick.preview_at != at {
                    return; // the arrows went on: this row is already behind
                }
                this.read_quick_preview(cx);
            });
        })
        .detach();
    }

    /// Asks for the file the cursor points at, unless it is already there.
    fn read_quick_preview(&mut self, cx: &mut Context<Self>) {
        if !self.quick.open || self.quick.mode != Mode::Files {
            return;
        }
        let (Some(worktree), Some(path)) = (self.active.clone(), self.quick_path()) else {
            // Nothing selected — an empty ranking, most often. The pane says
            // so rather than going on showing the file of a query one has
            // replaced.
            self.quick.preview = None;
            cx.notify();
            return;
        };
        if self
            .quick
            .preview
            .as_ref()
            .is_some_and(|shown| shown.path == path && shown.worktree == worktree)
        {
            return;
        }
        self.quick.preview = None;
        self.pending_preview = Some(crate::ui::search_view::PendingPreview {
            worktree: worktree.clone(),
            path: path.clone(),
            // The first line: a file asked for by name has no hit to come to
            // rest on.
            line: 1,
            // Nothing is lit. See `PendingPreview::marks`.
            marks: Default::default(),
            pane: crate::ui::search_view::PreviewPane::Quick,
        });
        self.git
            .send(crate::runtime::protocol::Cmd::ReadPreview { worktree, path });
        cx.notify();
    }

    /// The paths arrived while the palette was showing them: rank them again.
    ///
    /// Called from `project_files_arrived` and nowhere else — the list changes
    /// only when git answers.
    pub(super) fn rerank_quick(&mut self, cx: &mut Context<Self>) {
        if !self.quick.open || self.quick.mode != Mode::Files {
            return;
        }
        let query = self.quick_input.read(cx).value().to_string();
        self.rank_quick_files(&query, cx);
    }

    /// The arrows, in whichever list is showing.
    pub(super) fn step_quick(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        match self.quick.mode {
            Mode::Files => {
                let count = self.quick.hits.len();
                if count == 0 {
                    return;
                }
                // It stops at both ends rather than wrapping, as the search
                // panel's arrows do: a ranked list is read from the top down,
                // and coming back to the first row after the last is how one
                // reads the same file twice without noticing.
                let next = (self.quick.selected as isize + delta).clamp(0, count as isize - 1);
                self.quick.selected = next as usize;
                self.preview_quick_file(cx);
            }
            // The text side's preview is `sync_search_preview`'s, which
            // `step_search` already calls: it is the search's pane, shown here
            // and in the panel, and there is one of it.
            Mode::Text => self.step_search(delta, window, cx),
        }
        self.quick
            .scroll
            .scroll_to_item(self.quick_cursor(), gpui::ScrollStrategy::Top);
        cx.notify();
    }

    /// Which row the list is showing as selected.
    fn quick_cursor(&self) -> usize {
        match self.quick.mode {
            Mode::Files => self.quick.selected,
            Mode::Text => self.search.selected.unwrap_or(0),
        }
    }

    /// `Entrée`: the row one has landed on.
    ///
    /// **In text mode it may search instead**, and that is not two gestures
    /// under one key: under `search_view::MIN_AUTO` characters nothing goes
    /// out by itself, so there is no row to open and the key means the only
    /// thing left for it to mean. Once the answer is there, the same key opens
    /// what it found.
    pub(super) fn open_quick_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.quick.mode {
            Mode::Files => {
                let Some(path) = self.quick_path() else {
                    return;
                };
                self.close_quick(window, cx);
                self.leave_multiplex(cx);
                self.open_in_editor(path, cx);
            }
            Mode::Text => {
                if self.search_pending(cx) {
                    self.run_search(false, cx);
                    return;
                }
                if self.search.selected.is_none() {
                    return;
                }
                self.close_quick(window, cx);
                self.leave_multiplex(cx);
                self.open_search_row(window, cx);
            }
        }
    }

    /// The terminal grid goes away when the palette answers.
    ///
    /// The palette is reachable from the multiplexer — a modal covers whatever
    /// is under it, and this one is worth reaching from there: "which file was
    /// that" is a question one asks while watching five agents. But the answer
    /// is a file, and the grid replaces everything below the title bar, so an
    /// answer given without leaving it would open a tab nobody can see. The
    /// rule `work_in_worktree` already follows: acting on what one came to the
    /// grid for is leaving it.
    fn leave_multiplex(&mut self, cx: &mut Context<Self>) {
        if self.multiplex {
            self.multiplex = false;
            cx.notify();
        }
    }

    /// The file the cursor is on, in file mode.
    fn quick_path(&self) -> Option<PathBuf> {
        let hit = self.quick.hits.get(self.quick.selected)?;
        self.quick.paths.get(hit.index).cloned()
    }

    /// `Ctrl+Entrée`: the same results, in the panels that are made to be read.
    ///
    /// Only in text mode. The file side has no panel to hand anything to — its
    /// answer is a file, and opening it *is* the escape hatch.
    pub(super) fn expand_quick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.quick.mode != Mode::Text {
            return;
        }
        self.close_quick(window, cx);
        self.leave_multiplex(cx);
        // The list, beside whatever the centre holds — which the modal was
        // covering and which stays: nothing is written on the trail, exactly
        // as in `open_search`.
        self.reveal_panel(crate::ui::panels::SearchPanel::NAME, window, cx);
        // The list and not the field: the results are already there, and they
        // are what the gesture asked for.
        window.focus(&self.search_focus, cx);
        cx.notify();
    }

    /// `Shift Shift`, read from the root's modifier changes.
    ///
    /// Refused while another dialog is up: the settings, a commit
    /// confirmation, a question with a field in it — a palette dealt over one
    /// of those is a palette one has to dismiss before answering what one was
    /// answering.
    pub(super) fn quick_tapped(
        &mut self,
        event: &gpui::ModifiersChangedEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let modifiers = event.modifiers;
        let alone =
            !(modifiers.control || modifiers.alt || modifiers.platform || modifiers.function);
        if !self
            .quick
            .tap
            .modifiers(modifiers.shift, alone, std::time::Instant::now())
        {
            return;
        }
        if window.has_active_dialog(cx) {
            return;
        }
        self.open_quick(Mode::Files, window, cx);
    }

    // — Painting ————————————————————————————————————————————

    pub(super) fn render_quick(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mode = self.quick.mode;
        let head = self.render_quick_head(cx);
        let status = self.render_quick_status(cx);
        let list = match mode {
            Mode::Files => self.render_quick_files(window, cx).into_any_element(),
            // The panel's own empty state, word for word: it is the same
            // search, and a modal saying "nothing found" differently would be
            // a second answer to one question.
            Mode::Text if self.search.results.files.is_empty() => {
                self.render_search_empty(cx).into_any_element()
            }
            Mode::Text => self
                .render_search_rows(SCROLL, &self.quick.scroll.clone(), window, cx)
                .into_any_element(),
        };
        let pane = match mode {
            Mode::Files => PreviewPane::Quick,
            Mode::Text => PreviewPane::Search,
        };
        let preview = self.render_preview(pane, PREVIEW_SCROLL, window, cx);
        let border = cx.theme().border;
        // **The file beside the list and not under it.** Both are read
        // together — one walks the list to find out *which* of the answers it
        // is, and that question is answered by the code, not by the path — so
        // they are two columns, exactly as the search screen puts them and for
        // the same reason. Under it, the list would be six rows tall and the
        // file eight lines, which answers neither half.
        let body = h_flex()
            .size_full()
            .items_stretch()
            .child(div().w(LIST_WIDTH).flex_none().min_w_0().child(list))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .border_l_1()
                    .border_color(border)
                    .child(preview),
            );
        v_flex()
            // The palette's keys hang here, and nowhere else: `PREDICATE`
            // excludes `Dialog`, which is what every other shortcut of this
            // window wants and what this one cannot have.
            .key_context(crate::ui::shortcuts::quick_context())
            // **A click anywhere else answers "not this one".** The dialog's
            // own backdrop covers the window and closes on a press, so it
            // answers most of the gesture already; what it does not cover is
            // the dialog's own frame — the title row, the border, the padding
            // around this box — where a press is just as much a press outside
            // the palette. Both ways out end in `quick_closed`, which is what
            // matters: `Root::close_dialog` runs none of the `Dialog`'s own
            // callbacks, so this route has to say it for itself.
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close_quick(window, cx)))
            .size_full()
            .gap_1()
            .child(head)
            .children(status)
            .child(div().flex_1().min_h_0().child(body))
            .child(self.render_quick_hint(cx))
    }

    /// The two tabs and the field.
    fn render_quick_head(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = self.quick.mode;
        let tab = |target: Mode, label: SharedString, glyph: &'static str| {
            Button::new(("quick-tab", target as usize))
                .small()
                .icon(icon(glyph))
                .label(label)
                .selected(mode == target)
                .ghost()
        };
        v_flex()
            .w_full()
            .gap_1()
            .child(
                h_flex()
                    .w_full()
                    .gap_1()
                    .child(
                        tab(Mode::Files, tr!("quick-tab-files"), "file").on_click(cx.listener(
                            |this, _, window, cx| this.switch_quick(Mode::Files, window, cx),
                        )),
                    )
                    .child(
                        tab(Mode::Text, tr!("quick-tab-text"), "search").on_click(cx.listener(
                            |this, _, window, cx| this.switch_quick(Mode::Text, window, cx),
                        )),
                    ),
            )
            .child(Input::new(&self.quick_input))
    }

    /// One line saying what came back — the count, the cap, or why nothing did.
    ///
    /// The text side borrows the panel's wording rather than inventing its
    /// own: it is the same search, and two ways of saying "two thousand hits,
    /// list cut short" would be one too many.
    fn render_quick_status(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let (muted, danger) = (cx.theme().muted_foreground, cx.theme().danger);
        let (text, colour) = match self.quick.mode {
            Mode::Files => {
                let shown = self.quick.hits.len();
                if shown == 0 {
                    return None;
                }
                // What the list *is* changes with the field, so the line has
                // to say which: a count of files answers "your letters found
                // these", and the same count over the history would read as
                // the project holding fifty files.
                if self.quick_input.read(cx).value().is_empty() {
                    (tr!("quick-recent-count", { files: shown }), muted)
                } else {
                    (tr!("quick-file-count", { files: shown }), muted)
                }
            }
            Mode::Text => {
                if let Some(error) = self.search.error.clone() {
                    (SharedString::from(error), danger)
                } else if self.search.running {
                    (tr!("search-running"), muted)
                } else if self.search.results.total == 0 {
                    return None;
                } else {
                    let counted = tr!("search-count", {
                        hits: self.search.results.total,
                        files: self.search.results.files.len()
                    });
                    let text = if self.search.results.truncated {
                        format!("{counted} · {}", tr!("search-truncated"))
                    } else {
                        counted.to_string()
                    };
                    (SharedString::from(text), muted)
                }
            }
        };
        Some(
            div()
                .w_full()
                .px_1()
                .text_xs()
                .text_color(colour)
                .child(text),
        )
    }

    /// What the keys do, under the list.
    ///
    /// A modal has nowhere else to say it: its gestures are not on a rail, not
    /// in a menu, and not in the status bar, which it covers. `Ctrl+Entrée` in
    /// particular is the one thing here nobody would guess.
    fn render_quick_hint(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let text = match self.quick.mode {
            Mode::Files => tr!("quick-hint-files"),
            Mode::Text => tr!("quick-hint-text"),
        };
        div()
            .w_full()
            .px_1()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(text)
    }

    fn render_quick_files(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if self.quick.hits.is_empty() {
            return self.render_quick_empty(cx).into_any_element();
        }
        let hits = self.quick.hits.clone();
        let paths = self.quick.paths.clone();
        let selected = self.quick.selected;
        let look = FileLook::of(cx);
        let entity = cx.entity();
        let handle = self.quick.scroll.clone();
        let count = hits.len();
        let build = move |index: usize, cx: &mut App| {
            render_file_row(index, &hits, &paths, selected == index, &look, &entity, cx)
        };
        self.scrolled(
            SCROLL,
            &handle.clone(),
            crate::ui::motion::Axes::Vertical,
            window,
            uniform_list("quick-files", count, move |range, _window, cx| {
                range.map(|index| build(index, cx)).collect::<Vec<_>>()
            })
            .size_full()
            .track_scroll(&handle),
            cx,
        )
        .into_any_element()
    }

    fn render_quick_empty(&mut self, cx: &mut App) -> impl IntoElement {
        let empty = self.quick_input.read(cx).value().trim().is_empty();
        let message = if empty {
            tr!("quick-files-prompt")
        } else {
            tr!("find-no-match")
        };
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .text_color(cx.theme().muted_foreground)
            .child(icon("file"))
            .child(div().text_sm().px_4().text_center().child(message))
    }
}

/// What the theme gives a row, read once per frame and not per row.
#[derive(Clone)]
struct FileLook {
    row: gpui::Pixels,
    muted: gpui::Hsla,
    accent: gpui::Hsla,
    /// The background an occurrence is picked out with — the same one every
    /// other search in this window uses.
    hit: gpui::Hsla,
}

impl FileLook {
    fn of(cx: &App) -> Self {
        Self {
            row: crate::ui::theme::row_height(cx),
            muted: cx.theme().muted_foreground,
            accent: cx.theme().accent,
            hit: crate::ui::find::highlight_color(false, cx),
        }
    }
}

/// A ranked path: its icon, its name, and the folders leading to it.
///
/// **The name first and the folders after it**, which is not the order the
/// path is written in. One reads the list looking for a file name; the folders
/// are what tells two files of the same name apart, and that is a second
/// question, asked only of the handful of rows that carry it.
fn render_file_row(
    index: usize,
    hits: &Rc<Vec<quick::Hit>>,
    paths: &Rc<Vec<PathBuf>>,
    selected: bool,
    look: &FileLook,
    entity: &Entity<ClaudhubApp>,
    cx: &App,
) -> gpui::AnyElement {
    let Some(hit) = hits.get(index) else {
        return div().into_any_element();
    };
    let Some(path) = paths.get(hit.index) else {
        return div().into_any_element();
    };
    let Some(text) = path.to_str() else {
        return div().into_any_element();
    };
    let at = quick::name_at(text);
    let (folders, name) = quick::split(&hit.ranges, at);
    let entity = entity.clone();
    h_flex()
        .id(("quick-file", index))
        .h(look.row)
        .w_full()
        .pl_1()
        .pr(crate::ui::theme::scroll_gutter())
        .gap_2()
        .items_center()
        .cursor_pointer()
        .when(selected, |el| el.bg(look.accent.opacity(0.4)))
        .hover(|s| s.bg(look.accent.opacity(0.3)))
        // A single click selects and previews, a double one opens: the same
        // rule as the result list beside it, and what makes the pane on the
        // right worth having — one browses a ranking with the mouse as much as
        // with the arrows.
        .on_click(move |event, window, cx| {
            let open = event.click_count() > 1;
            entity.update(cx, |this, cx| {
                this.quick.selected = index;
                if open {
                    this.open_quick_row(window, cx);
                } else {
                    this.preview_quick_file(cx);
                    cx.notify();
                }
            });
        })
        .child(crate::ui::file_icons::file_icon(path, cx))
        .child(
            div()
                .flex_none()
                .text_xs()
                .child(marked(&text[at..], &name, look.hit)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(look.muted)
                .child(marked(&text[..at], &folders, look.hit)),
        )
        .into_any_element()
}

/// A fragment of path with the matched characters picked out.
///
/// `highlight::overlay` over an empty base rather than a style built here:
/// what a match looks like is decided in one place for the whole window, and a
/// second opinion about it would only be noticed once the theme changed.
fn marked(text: &str, ranges: &[std::ops::Range<usize>], colour: gpui::Hsla) -> gpui::AnyElement {
    let text = SharedString::from(text.to_string());
    if ranges.is_empty() {
        return div().child(text).into_any_element();
    }
    let marks: Vec<_> = ranges.iter().map(|range| (range.clone(), colour)).collect();
    StyledText::new(text)
        .with_highlights(crate::ui::highlight::overlay(&[], &marks))
        .into_any_element()
}

impl Mode {
    /// What the field announces while it is empty.
    fn placeholder(self) -> SharedString {
        match self {
            Mode::Files => tr!("quick-files-placeholder"),
            Mode::Text => tr!("search-placeholder"),
        }
    }
}
