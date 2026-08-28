//! Searching inside a panel.
//!
//! Almost everything Claudhub shows is a list: files, branches, commits,
//! issues, notes, diff lines. A list that cannot be queried has to be scanned
//! by eye, and a Laravel project has forty thousand entries.
//!
//! **One gesture, two behaviours.** Where the list is free to order itself, the
//! search **filters**: what does not match disappears, and what is left is what
//! was being looked for. Where the order carries meaning — the diff, which is
//! the file; the history, whose graph links a row to its neighbours — it
//! **jumps** from one occurrence to the next without removing anything.
//! Filtering a commit graph would make every line point at the wrong row.
//!
//! **Case is derived from the query** (smart case): an all-lowercase query
//! ignores case, a query carrying a capital respects it. It is every editor's
//! convention, and it saves one more button for a setting that changes with
//! every search.

use std::collections::HashMap;
use std::ops::Range;

use gpui::{div, prelude::*, Context, Entity, Focusable, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    ActiveTheme, Sizable,
};

use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;

/// The panels that know how to search.
///
/// The terminal is not among them: its content is a program's screen, which has
/// its own `Ctrl+F` — and an alacritty grid's scrollback is not a list we hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pane {
    Files,
    Db,
    Changes,
    Branch,
    History,
    /// The repository's tags. It filters: the list's order is ours — by date of
    /// tag — and nothing in it links one row to the next.
    Tags,
    /// The work put aside. It filters, for the tags' reason: the order is the
    /// stack's, and nothing in it links one row to the next.
    Stashes,
    /// The Pest suite. It filters: narrowing two thousand rows to the one
    /// worth running is the panel's whole point.
    Tests,
    /// The run being followed. Its content is a process's account; the key
    /// exists so `Ctrl+F` there does not go to the panel touched before it.
    TestRun,
    Notes,
    Conflicts,
    Diff,
    /// The built-in editor. It has `InputState`'s search, not ours — but it
    /// needs its key like the others, if only so `Ctrl+F` does not go to the
    /// panel touched before it.
    Editor,
    /// The SQL console. Same thing: the query editor is what searches.
    Console,
    /// The queries already run. It filters, like every list whose order is
    /// ours: what is left is what was being looked for.
    SqlHistory,
    /// The project-wide search. Its field **is** the search, so `Ctrl+F` there
    /// focuses it rather than opening a second bar over it — see
    /// `ClaudhubApp::open_find`.
    Search,
    /// The file shown beside the results. Same thing: what one searches from
    /// this screen is the project, not the preview.
    SearchPreview,
}

impl Pane {
    /// Does the panel jump from one occurrence to the next instead of filtering.
    fn jumps(self) -> bool {
        matches!(self, Pane::Diff | Pane::History)
    }

    /// What the bar announces while it is empty.
    fn placeholder(self) -> SharedString {
        match self {
            Pane::Diff => tr!("find-in-diff"),
            Pane::History => tr!("find-in-history"),
            _ => tr!("find-placeholder"),
        }
    }
}

/// What a panel's search is filed under.
///
/// **A panel's search belongs to the checkout being looked at**, like its
/// editors, its terminals and its trail: the entries it filters are that
/// project's, and a filter carried from one worktree to the next hides files in
/// a tree that has nothing to do with what was typed — without saying so, since
/// the bar reads as belonging to the panel on screen.
///
/// `None` for the moment where no worktree is shown: the panels are painted
/// then too, and a key they cannot be filed under would give them one another's.
pub type Key = (Option<std::path::PathBuf>, Pane);

/// A panel's search: its field, and whether it is open.
pub struct Finder {
    /// Created **once**, on first opening. Recreated at render time, it would
    /// lose the cursor and the text on the first keystroke.
    pub input: Entity<InputState>,
    pub open: bool,
}

/// Does the query match the text?
pub fn matches(query: &str, haystack: &str) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    first_match(query, haystack, 0).is_some()
}

/// Every occurrence, as **byte** offsets — that is what gpui expects to style a
/// fragment of text, and indexing by characters breaks at the first accent.
pub fn find_all(query: &str, haystack: &str) -> Vec<Range<usize>> {
    let query = query.trim();
    let mut out = Vec::new();
    if query.is_empty() {
        return out;
    }
    let mut from = 0;
    while let Some(range) = first_match(query, haystack, from) {
        // An empty occurrence would loop forever: `first_match` returns none,
        // the query never being empty here.
        from = range.end;
        out.push(range);
    }
    out
}

/// The first occurrence at or after `from` — what vim's `/` and `n` step with,
/// so that both searches read case the same way.
pub fn find_from(query: &str, haystack: &str, from: usize) -> Option<Range<usize>> {
    let query = query.trim();
    if query.is_empty() {
        return None;
    }
    first_match(query, haystack, from)
}

/// The first occurrence from a given offset.
///
/// A character-by-character comparison rather than a search inside
/// `to_lowercase()`: lowercasing changes the byte length of some characters,
/// and the offsets returned would no longer point at anything in the original
/// text.
fn first_match(query: &str, haystack: &str, from: usize) -> Option<Range<usize>> {
    let sensitive = query.chars().any(char::is_uppercase);
    let first = query.chars().next()?;
    // Sliced and not skipped: `find_all` restarts from the end of the previous
    // occurrence, and walking the whole text again each time made it quadratic.
    // `from` is always a character boundary — it comes from a match's end.
    for (offset, candidate) in haystack[from..].char_indices() {
        let start = from + offset;
        if !same(candidate, first, sensitive) {
            continue;
        }
        let mut end = start;
        let mut hay = haystack[start..].chars();
        let mut ok = true;
        for wanted in query.chars() {
            match hay.next() {
                Some(c) if same(c, wanted, sensitive) => end += c.len_utf8(),
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            return Some(start..end);
        }
    }
    None
}

fn same(a: char, b: char, sensitive: bool) -> bool {
    if sensitive {
        a == b
    } else {
        a == b || a.to_lowercase().eq(b.to_lowercase())
    }
}

impl ClaudhubApp {
    /// A panel's query, empty while its bar is closed.
    ///
    /// Empty and not `None`: the callers all filter the same way, and an empty
    /// query removes nothing.
    pub(super) fn query(&self, pane: Pane, cx: &gpui::App) -> String {
        self.finders
            .get(&self.find_key(pane))
            .filter(|finder| finder.open)
            .map(|finder| finder.input.read(cx).value().to_string())
            .unwrap_or_default()
    }

    /// Where a panel's search is filed: the worktree on show, and the panel.
    fn find_key(&self, pane: Pane) -> Key {
        (self.active.clone(), pane)
    }

    /// Drops the searches of a checkout that is gone, as its trail is dropped.
    pub(super) fn forget_finders(&mut self, worktree: &std::path::Path) {
        self.finders
            .retain(|(kept, _), _| kept.as_deref() != Some(worktree));
    }

    /// Records the panel where the gesture happened. That is what `Ctrl+F` aims at.
    pub(super) fn touch_pane(&mut self, pane: Pane, cx: &mut Context<Self>) {
        if self.pane != pane {
            self.pane = pane;
            cx.notify();
        }
    }

    /// Opens the target panel's bar and gives it the focus.
    pub(super) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.pane;
        // The search screen has one field, and it is the search: a second bar
        // over it would be two places to type the same thing.
        if matches!(pane, Pane::Search | Pane::SearchPreview) {
            self.open_search(window, cx);
            return;
        }
        self.open_find_in(pane, window, cx);
    }

    /// Opens **a named panel's** bar, whichever one the last click was in.
    ///
    /// Split out for the file search: `Ctrl+P` says which panel it wants —
    /// the explorer — where `Ctrl+F` asks the click. Returns the field so the
    /// caller can seed it.
    fn open_find_in(
        &mut self,
        pane: Pane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let key = self.find_key(pane);
        let input = match self.finders.get_mut(&key) {
            Some(finder) => {
                finder.open = true;
                finder.input.clone()
            }
            None => {
                let placeholder = pane.placeholder();
                let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
                // A keystroke changes the displayed list: without this
                // subscription, the panel would keep the earlier picture.
                // Enter is NOT subscribed to here: a single-line field
                // propagates the key after emitting `PressEnter`, so the
                // `enter`/`shift-enter` bindings on the bar fire anyway —
                // subscribing too made Enter step twice, and Shift+Enter
                // step forward and back, which is not at all.
                cx.subscribe(&input, move |this, _, event, cx| {
                    if let InputEvent::Change = event {
                        if pane.jumps() {
                            this.find_reset(pane);
                        }
                        cx.notify();
                    }
                })
                .detach();
                self.finders.insert(
                    key,
                    Finder {
                        input: input.clone(),
                        open: true,
                    },
                );
                input
            }
        };
        input.focus_handle(cx).focus(window, cx);
        cx.notify();
        input
    }

    /// Searching the project's files **by name**, from anywhere.
    ///
    /// The explorer's own bar, reached without going through the click that
    /// `Ctrl+F` reads: one goes to the editing screen, the tree shows itself,
    /// and the field takes the keyboard. `self.pane` follows, otherwise `Esc`
    /// and the arrows would still be aimed at the panel touched before.
    ///
    /// The seed is the project-wide search's, and deliberately so — the same
    /// gesture with the same answer to "what was selected", `Ctrl+P` looking
    /// for the file where `Ctrl+Shift+F` looks for the text. It is read
    /// **before** the tree comes forward: `reveal_panel` moves the focus, and
    /// the focus is what says which surface the selection is in.
    pub(super) fn open_file_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let seed = self.search_seed(window, cx);
        // The tree and not the centre: what this searches is the file list, one
        // tab of the column one picks from.
        self.reveal_panel(crate::ui::panels::FilesPanel::NAME, window, cx);
        self.pane = Pane::Files;
        let input = self.open_find_in(Pane::Files, window, cx);
        if let Some(text) = seed {
            input.update(cx, |state, cx| {
                // Selected whole, like the project search's field: the seed is
                // an offer, and the next keystroke replaces it.
                state.set_value(text, window, cx);
                state.select_all(window, cx);
            });
            // `set_value` emits no change event, so the filtered tree would
            // keep the previous picture without this.
            cx.notify();
        }
    }

    /// Closes the target panel's bar.
    pub(super) fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.find_key(self.pane);
        if let Some(finder) = self.finders.get_mut(&key) {
            finder.open = false;
        }
        // The focus goes back to the view: leaving it in a field just hidden
        // would make the review arrows inert.
        self.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// The current occurrence changes in the panels that jump.
    pub(super) fn find_step(&mut self, delta: isize, cx: &mut Context<Self>) {
        match self.pane {
            Pane::Diff => self.step_diff_match(delta, cx),
            Pane::History => self.step_history_match(delta, cx),
            _ => {}
        }
    }

    fn find_reset(&mut self, pane: Pane) {
        if pane == Pane::Diff {
            self.diff_search.valid = false;
        }
    }

    /// A panel's search bar, when it is open.
    ///
    /// It sits under the panel header and not over the list: a floating band
    /// would cover the first entries, which are precisely the ones a search
    /// brings to the top.
    pub(super) fn render_find(
        &mut self,
        pane: Pane,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let finder = self
            .finders
            .get(&self.find_key(pane))
            .filter(|finder| finder.open)?;
        let input = finder.input.clone();
        let query = input.read(cx).value().to_string();
        let count = self.find_count(pane, &query);

        Some(
            h_flex()
                // The context only exists under this bar: `Esc` closes the
                // search here and has nothing to close elsewhere.
                .key_context(crate::ui::shortcuts::find_context())
                .h(crate::ui::theme::bar_height(cx))
                .w_full()
                .px_1()
                .gap_1()
                .items_center()
                .border_b_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().secondary)
                .child(icon("search").xsmall())
                .child(div().flex_1().child(Input::new(&input).xsmall()))
                .when_some(count, |el, (current, total)| {
                    el.child(
                        div()
                            .px_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(SharedString::from(if total == 0 {
                                tr!("find-no-match").to_string()
                            } else {
                                format!("{}/{total}", current + 1)
                            })),
                    )
                })
                .when(pane.jumps(), |el| {
                    el.child(
                        Button::new("find-prev")
                            .ghost()
                            .xsmall()
                            .icon(icon("arrow-up"))
                            .tooltip(tr!("find-previous"))
                            .on_click(cx.listener(|this, _, _window, cx| this.find_step(-1, cx))),
                    )
                    .child(
                        Button::new("find-next")
                            .ghost()
                            .xsmall()
                            .icon(icon("arrow-down"))
                            .tooltip(tr!("find-next"))
                            .on_click(cx.listener(|this, _, _window, cx| this.find_step(1, cx))),
                    )
                })
                .child(
                    Button::new("find-close")
                        .ghost()
                        .xsmall()
                        .icon(icon("x"))
                        .tooltip(tr!("find-close"))
                        .on_click(cx.listener(|this, _, window, cx| this.close_find(window, cx))),
                ),
        )
    }

    /// The count the bar shows.
    ///
    /// Only the diff carries it: it is the only list whose search effect is not
    /// visible — a filter leaves what it found in plain sight, whereas an
    /// occurrence may be four thousand lines away. The history, for its part,
    /// dims what does not match: the count reads off the screen.
    fn find_count(&mut self, pane: Pane, query: &str) -> Option<(usize, usize)> {
        if pane != Pane::Diff || query.trim().is_empty() {
            return None;
        }
        self.refresh_diff_search(query);
        Some((self.diff_search.current, self.diff_search.hits.len()))
    }
}

/// The occurrences found in the displayed diff.
///
/// They are computed on every query change and on every diff arrival, **never
/// at render time**: a virtualised list's closure runs for each visible line on
/// each frame.
#[derive(Default)]
pub struct DiffSearch {
    /// The query `hits` was computed for.
    pub query: String,
    /// False when a new diff has arrived: the offsets refer to text that is no
    /// longer on screen.
    pub valid: bool,
    /// The occurrences in file order.
    pub hits: std::rc::Rc<Vec<Hit>>,
    /// The same, filed by line: that is how rendering looks them up, and it
    /// does so for every visible line.
    pub by_line: MatchesByLine,
    pub current: usize,
    /// Has `current` been brought into view since the query changed. False on
    /// a fresh search, so the first Enter reveals the first occurrence instead
    /// of stepping past it — an occurrence may be four thousand lines away,
    /// and typing alone never scrolls.
    pub landed: bool,
}

/// A line's occurrences, filed by `(hunk, line)`.
pub type MatchesByLine = std::rc::Rc<HashMap<(usize, usize), Vec<Range<usize>>>>;

/// An occurrence: the diff line it is on, and its place in that line's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub hunk: usize,
    pub line: usize,
    pub range: Range<usize>,
}

/// The background of an occurrence, laid over the syntax highlighting.
pub fn highlight_color(current: bool, cx: &gpui::App) -> gpui::Hsla {
    if current {
        cx.theme().warning
    } else {
        cx.theme().warning.opacity(0.35)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_all_lowercase_query_ignores_case() {
        assert!(matches("todo", "TODO: rewrite"));
        assert!(matches("REWRITE", "TODO: REWRITE"));
    }

    #[test]
    fn a_query_with_a_capital_respects_it() {
        assert!(!matches("Todo", "todo: rewrite"));
        assert!(matches("Todo", "Todo: rewrite"));
    }

    #[test]
    fn an_empty_query_matches_everything() {
        assert!(matches("", "anything at all"));
        assert!(matches("   ", "anything at all"));
        assert!(find_all("", "anything at all").is_empty());
    }

    /// The offsets are byte offsets: a case-insensitive search must not shift
    /// them by an accent.
    #[test]
    fn offsets_are_byte_offsets_even_past_an_accent() {
        let text = "été chaud";
        let hits = find_all("chaud", text);
        assert_eq!(hits, vec![6..11]);
        assert_eq!(&text[hits[0].clone()], "chaud");
    }

    #[test]
    fn a_repeated_needle_is_found_every_time() {
        assert_eq!(find_all("ab", "abcab"), vec![0..2, 3..5]);
    }

    /// Every occurrence is found from the end of the previous one, and the
    /// offsets stay byte offsets past a multi-byte character: that is what the
    /// resumed scan must not break.
    #[test]
    fn the_scan_resumes_where_the_last_occurrence_ended() {
        let text = "éaébéc";
        let hits = find_all("é", text);
        assert_eq!(hits, vec![0..2, 3..5, 6..8]);
        for hit in &hits {
            assert_eq!(&text[hit.clone()], "é");
        }
    }

    /// Two overlapping occurrences are not returned twice: the ranges have to
    /// stay disjoint for gpui to accept them.
    #[test]
    fn overlapping_occurrences_do_not_overlap_in_the_result() {
        assert_eq!(find_all("aa", "aaaa"), vec![0..2, 2..4]);
    }

    #[test]
    fn a_needle_longer_than_the_line_is_not_found() {
        assert!(find_all("abcdef", "abc").is_empty());
    }
}
