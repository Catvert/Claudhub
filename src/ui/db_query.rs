//! The SQL console.
//!
//! An editor at the top, the result underneath: it is PhpStorm's console, and it
//! is the shape of every one already under our fingers.
//!
//! **A console is a document**, one panel each, the way an open file is: the
//! centre is a tab group and the dock's bar is the tab bar. It was a single
//! panel taking the diff's place, because the centre was one slot and two
//! stacked consoles would have needed a tab bar of our own; the one dock area
//! gave us that bar, so the count is whatever one opens.
//!
//! **Opening a table replaces the console one is in**, and does not add one:
//! browsing a schema with the mouse would otherwise leave ten tabs behind. A
//! console is added by asking for one — the `+` of the databases panel,
//! `Ctrl+Shift+Q`, or "open in a new tab" on a table.
//!
//! ## The result window
//!
//! What is displayed is not "page *n*" but a **window** onto the result: it
//! starts at `offset`, it counts `shown` rows, and it **grows** when scrolling
//! reaches the bottom (`load_more`). The two paging gestures move it by a block,
//! scrolling extends it — and in both cases it is the same request, at a
//! different `offset`.
//!
//! That is what makes it possible to browse a million rows without ever loading
//! more than has been read, and without the context jump a "next page" imposes
//! on the eye in the middle of a read.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use gpui::{
    div, prelude::*, px, App, Context, Entity, Focusable as _, SharedString, Task, WeakEntity,
    Window,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::{CompletionProvider, Editor, Rope, RopeExt as _},
    menu::{DropdownMenu as _, PopupMenuItem},
    resizable::{resizable_panel, v_resizable, ResizableState},
    table::{Column, ColumnSort, DataTable, TableDelegate, TableState},
    v_flex, ActiveTheme, Disableable, Sizable,
};
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    TextEdit,
};

use crate::db;
use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::icons::icon;
use crate::ui::motion::Axes;
use crate::ui::settings::Settings;
use crate::ui::surface::Surface;

/// The window sizes the bar offers.
///
/// Four values rather than an input field: it is an order of magnitude one
/// chooses — "enough to see", "enough to search" — and not a number adjusted to
/// the unit.
const PAGE_SIZES: [usize; 4] = [100, 500, 1_000, 5_000];

/// The sort asked of the console: one column, one direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    /// The column's index in the result, which is also its rank in the
    /// `ORDER BY` `db::order_by` writes.
    pub column: usize,
    pub ascending: bool,
}

/// A rectangle of cells, as the mouse draws it.
///
/// **An anchor and a cursor, and not two ordered corners**: it is the anchor a
/// Shift+click keeps and the cursor it moves, and ordering them at construction
/// would lose which end the selection started from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (usize, usize),
    pub cursor: (usize, usize),
}

impl Selection {
    fn cell(row: usize, column: usize) -> Self {
        Self {
            anchor: (row, column),
            cursor: (row, column),
        }
    }

    fn rows(&self) -> std::ops::RangeInclusive<usize> {
        self.anchor.0.min(self.cursor.0)..=self.anchor.0.max(self.cursor.0)
    }

    fn columns(&self) -> std::ops::RangeInclusive<usize> {
        self.anchor.1.min(self.cursor.1)..=self.anchor.1.max(self.cursor.1)
    }

    fn contains(&self, row: usize, column: usize) -> bool {
        self.rows().contains(&row) && self.columns().contains(&column)
    }

    /// The number of cells, which decides the shape of the copy: a single one
    /// comes out as it is, several come out in columns.
    fn count(&self) -> usize {
        (self.rows().count()) * (self.columns().count())
    }
}

/// What the console shows and what it waits for.
#[derive(Default)]
pub struct QueryState {
    /// The console's connection, when one is open. `None`: the central panel
    /// shows the diff.
    pub connection: Option<db::Connection>,
    /// The current database, the one a `USE` would choose. `None` for SQLite,
    /// which has only one.
    pub database: Option<String>,
    /// The query as it went out.
    ///
    /// It is what paging, sorting and exporting replay, and not the editor's
    /// text: one keeps typing while a query runs, and what follows has to be
    /// about what is on screen.
    pub sent: Option<String>,
    /// The sort asked for, applied by the engine around `sent`.
    pub sort: Option<Sort>,
    /// The query lets itself be sorted — see `db::can_order`. Only known once
    /// the columns have come back, hence its place here rather than at send time.
    pub can_sort: bool,
    /// The last request. It is what identifies the answer being waited for:
    /// changing page, sorting and extending all replay the same text.
    pub request: u64,
    /// The running request extends the window instead of replacing it.
    pub appending: bool,
    /// The running request is a query one **asked for**, and its answer goes
    /// into the history.
    ///
    /// Paging, sorting and scrolling to the bottom all replay the same text:
    /// filing them would count four runs for one question asked, and a row's
    /// "×4" is precisely what says a query was worth running again.
    pub record: bool,
    pub running: bool,
    pub error: Option<SharedString>,
    /// What the displayed window reports, for the status bar and the paging. The
    /// rows themselves live in the table's delegate.
    pub offset: usize,
    pub shown: usize,
    pub more: bool,
    pub affected: Option<u64>,
    pub has_columns: bool,
    pub elapsed_ms: u64,
    /// The export that has gone out and not come back, by the path it writes.
    ///
    /// The path and not a flag: two consoles can be exporting at once, and the
    /// answer has to find the one that asked. It is what the worker gives back,
    /// so nothing had to be added to the wire for it.
    pub exporting: Option<std::path::PathBuf>,
}

/// Which console a gesture is about.
///
/// A number and not a rank: the rank is what the tab says and it is given back
/// when a console closes, so two consoles would share one over a session. What
/// names a console has to outlive every other console.
///
/// `ConsoleId(0)` names none: the counter starts at one, so the `Default` an
/// empty result grid carries points at nothing rather than at the first
/// console.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConsoleId(pub u64);

/// Where a table asked for is opened.
///
/// The whole of the difference between the two gestures, and the reason there
/// is an enum rather than a `bool` at fifteen call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleTarget {
    /// The console one is in, or a fresh one where there is none. What a click
    /// in the schema tree means: browsing a schema with the mouse must not
    /// leave ten tabs behind.
    Current,
    /// A console of its own, whatever is open.
    NewTab,
}

/// One SQL console, as the window holds it.
///
/// **One console was the whole window's**, and the central slot was unique:
/// opening a table replaced the query one was reading. The centre is a tab
/// group like any other now — a console is a document among the documents, the
/// way a file is — so there are as many as one opens.
///
/// Everything a console needs to survive a frame is here and not in
/// `ClaudhubApp`: an entity created once, per the gpui rule, and the rule now
/// reads "once per console". `state` is the exception in the other direction —
/// it is emptied whenever a query is replaced, which is why the decoration
/// layers of `host` are beside it rather than in it.
pub struct Console {
    pub id: ConsoleId,
    /// What the tab says: `SQL 1`. The **lowest free** rank at opening, so
    /// three open consoles are 1, 2 and 3 rather than 7, 9 and 12.
    pub rank: usize,
    /// The worktree it belongs to, as a terminal carries its own: the tabs of
    /// the tree one is not looking at stay in the dock, invisible, keeping the
    /// place they were given.
    pub worktree: std::path::PathBuf,
    pub state: QueryState,
    /// The console's editor. Created **once**: recreated at render time, it
    /// would lose the cursor, the selection and the text on the first keystroke.
    pub input: Entity<gpui_component::input::EditorState>,
    /// The modal harness — the same one the file editor holds, since SQL is
    /// code read and written the same way.
    pub host: crate::ui::surface::VimHost,
    /// The names this console completes, shared with the completion provider
    /// its editor holds. Per console and not per window: two consoles can be
    /// open on two databases, and offering one's tables in the other is worse
    /// than offering none.
    pub schema: Rc<RefCell<SchemaIndex>>,
    /// The result table. An entity created once as well: rebuilding it on every
    /// query would lose the column widths just adjusted with the mouse.
    pub table: Entity<TableState<Results>>,
    /// The editor/results split.
    pub split: Entity<ResizableState>,
    pub panel: Entity<crate::ui::panels::QueryPanel>,
}

/// What a console's tab says.
///
/// A free function and not a method: the title is handed to the panel when it
/// is built, which happens **before** the console is pushed — the panel cannot
/// read an application it is being built inside an update of.
pub fn console_title(rank: usize) -> SharedString {
    SharedString::from(format!("SQL {rank}"))
}

/// The names the console knows how to complete.
///
/// Filed behind a `RefCell` because the completion provider is an
/// `Rc<dyn CompletionProvider>` the editor holds, and it has to be fillable
/// afterwards — indexing a schema arrives several seconds after the console
/// opens.
#[derive(Default)]
pub struct SchemaIndex {
    /// The database this index corresponds to: a console reopened elsewhere must
    /// not offer the previous one's tables.
    pub database: Option<String>,
    /// `(table, columns)`, in schema order.
    pub tables: Vec<(String, Vec<String>)>,
    /// The schema's foreign keys, which say which result column can be
    /// followed. Filed beside the names rather than derived from them: it is
    /// the same answer that fills both, and one projection each is made where
    /// it arrives.
    pub foreign_keys: Vec<db::link::Key>,
}

/// A query's result, as the table reads it.
///
/// The delegate **is** the result: gpui-component's table asks for its cells one
/// by one as the scrolling goes, and giving it anything other than direct access
/// to the rows would make one copy per visible cell per frame.
#[derive(Default)]
pub struct Results {
    pub rows: db::Rows,
    widths: Vec<gpui::Pixels>,
    mono: Option<gpui::SharedString>,
    /// The sort in force, which decides the headers' arrow.
    sort: Option<Sort>,
    /// The headers react to a click.
    sortable: bool,
    /// The result continues past the window: it is what allows scrolling to ask
    /// for more of it.
    more: bool,
    /// A page has already gone out. Without this guard, every frame spent at the
    /// bottom of the list would ask for another.
    loading: bool,
    /// The selected rectangle of cells, if there is one.
    ///
    /// **The selection is ours and not the table's.** gpui-component's knows
    /// only one cell (`selected_cell`), whereas what one copies from a result
    /// grid is almost always a whole column or a block. Two mechanisms would
    /// fight over the click and the background colour; there is therefore only
    /// one, and `cell_selectable` stays off.
    selection: Option<Selection>,
    /// A drag is under way: the hovered cells extend the rectangle.
    dragging: bool,
    /// What each column of the result points at, when it is a foreign key —
    /// one entry per column, computed when the rows arrive. Never in the render
    /// closure, which runs for every visible cell of every frame.
    links: Vec<Option<db::link::Target>>,
    /// The engine, which decides how the query written by a jump quotes.
    engine: db::Engine,
    /// The system key is held: the cells that can be followed say so.
    ///
    /// Kept here rather than read from the application at each cell: the
    /// delegate is asked for its cells one by one on every frame, and what
    /// changes is a flag that flips twice per gesture. `ClaudhubApp::
    /// arm_db_follow` is what pushes it, from the same listener the diff's
    /// underline hangs on — see `ClaudhubApp::follow_armed`.
    armed: bool,
    /// The application, to report a sort or a request for more to it.
    ///
    /// **Weak**, like the dock's panels: the application holds the table, and a
    /// strong reference would close the cycle.
    app: Option<WeakEntity<ClaudhubApp>>,
    /// Whose grid this is.
    ///
    /// Carried and not looked up: a sort, a right click and a scroll to the
    /// bottom all report back to the application, and "the console in front" is
    /// not the answer — two consoles can be side by side in a split, and the
    /// one being scrolled is not necessarily the one with the focus.
    console: ConsoleId,
}

/// A cell's width, derived from the content.
///
/// Measured on the first fifty rows only: a window holds a thousand, and the
/// window's widest column is not the one being looked at. Bounded on both sides
/// — an `id` column must not be a thread, and a ten-thousand-character `TEXT`
/// must not push all the others out of sight.
fn column_width(rows: &db::Rows, index: usize) -> gpui::Pixels {
    let mut chars = rows
        .columns
        .get(index)
        .map_or(0, |name| name.chars().count());
    for row in rows.rows.iter().take(50) {
        if let Some(Some(value)) = row.get(index) {
            chars = chars.max(value.chars().count());
        }
    }
    px((chars as f32 * 7.5 + 40.).clamp(80., 420.))
}

impl Results {
    fn new(
        console: ConsoleId,
        rows: db::Rows,
        links: Vec<Option<db::link::Target>>,
        state: &QueryState,
        cx: &Context<ClaudhubApp>,
    ) -> Self {
        let widths = (0..rows.columns.len())
            .map(|index| column_width(&rows, index))
            .collect();
        Self {
            more: rows.more,
            rows,
            widths,
            mono: Some(cx.theme().mono_font_family.clone()),
            sort: state.sort,
            sortable: state.can_sort,
            loading: false,
            selection: None,
            dragging: false,
            links,
            engine: state
                .connection
                .as_ref()
                .map(|connection| connection.engine)
                .unwrap_or_default(),
            armed: false,
            app: Some(cx.weak_entity()),
            console,
        }
    }

    /// The sort a click on `column` asks for: ascending, then descending, then
    /// no sort at all.
    ///
    /// **The table offers its own sequence and it is ignored.** Its own starts
    /// from descending, which is surprising on a result grid; and above all it
    /// lives in its own state, which `refresh` rebuilds from `column()` on every
    /// result. Only one of the two memories can be authoritative, and it is the
    /// console's — it is what decides the query sent.
    fn next_sort(&self, column: usize) -> Option<Sort> {
        match self.sort {
            Some(sort) if sort.column == column && sort.ascending => Some(Sort {
                column,
                ascending: false,
            }),
            Some(sort) if sort.column == column => None,
            _ => Some(Sort {
                column,
                ascending: true,
            }),
        }
    }

    /// A click sets the selection, or extends it if Shift is held.
    ///
    /// The drag is armed here: it is the press that starts a selection, not the
    /// release — otherwise it could not be dragged out.
    fn press(&mut self, row: usize, column: usize, extend: bool) {
        self.selection = match (extend, self.selection) {
            (true, Some(selection)) => Some(Selection {
                cursor: (row, column),
                ..selection
            }),
            _ => Some(Selection::cell(row, column)),
        };
        self.dragging = true;
    }

    /// Extends the selection as the mouse passes. Returns true if something
    /// moved — repainting on every pixel crossed would be work for nothing.
    fn drag_to(&mut self, row: usize, column: usize) -> bool {
        let Some(selection) = self.selection.as_mut() else {
            return false;
        };
        if selection.cursor == (row, column) {
            return false;
        }
        selection.cursor = (row, column);
        true
    }

    /// The whole loaded result, corner to corner.
    fn select_all(&mut self) {
        let (rows, columns) = (self.rows.rows.len(), self.rows.columns.len());
        self.selection = match (rows, columns) {
            (0, _) | (_, 0) => None,
            (rows, columns) => Some(Selection {
                anchor: (0, 0),
                cursor: (rows - 1, columns - 1),
            }),
        };
    }

    /// The text of what is selected, ready for the clipboard.
    ///
    /// **A single cell comes out as it is**: it is an identifier that will be
    /// pasted into another query, not a table — quoting it would be one more
    /// chore on every paste. Several cells come out in tab-separated columns.
    fn selected_text(&self, headers: bool) -> Option<String> {
        let selection = self.selection?;
        if !headers && selection.count() == 1 {
            let (row, column) = selection.anchor;
            return Some(self.cell(row, column).cloned().unwrap_or_default());
        }
        let mut out = String::new();
        if headers {
            out.push_str(&db::tsv_line(selection.columns().map(|column| {
                self.rows.columns.get(column).map(|name| name.as_str())
            })));
        }
        for row in selection.rows() {
            out.push_str(&db::tsv_line(
                selection
                    .columns()
                    .map(|column| self.cell(row, column).map(|value| value.as_str())),
            ));
        }
        Some(out)
    }

    /// The whole loaded result, header included.
    fn all_text(&self) -> String {
        let mut out = db::tsv_line(self.rows.columns.iter().map(|name| Some(name.as_str())));
        for row in &self.rows.rows {
            out.push_str(&db::tsv_line(row.iter().map(|cell| cell.as_deref())));
        }
        out
    }

    /// A whole row, header included — it is what gets read back in a message
    /// when asking "look at this record".
    fn row_text(&self, row: usize) -> Option<String> {
        let cells = self.rows.rows.get(row)?;
        let mut out = db::tsv_line(self.rows.columns.iter().map(|name| Some(name.as_str())));
        out.push_str(&db::tsv_line(cells.iter().map(|cell| cell.as_deref())));
        Some(out)
    }

    /// What a cell follows: the row its value names in another table.
    ///
    /// A `NULL` names nothing, and a column that is not a key names nothing
    /// either — in both cases there is no entry to offer rather than one that
    /// would answer with an empty result.
    fn link(&self, row: usize, column: usize) -> Option<(&db::link::Target, &String)> {
        let target = self.links.get(column)?.as_ref()?;
        Some((target, self.cell(row, column)?))
    }

    fn cell(&self, row: usize, column: usize) -> Option<&String> {
        self.rows.rows.get(row)?.get(column)?.as_ref()
    }

    /// Reports a table gesture to the application.
    ///
    /// **Deferred**, and that is not a precaution: the table calls its delegate
    /// in the middle of an `update` on itself, and the application answers by
    /// replacing that delegate — so by re-borrowing the entity currently
    /// borrowed, which gpui refuses with a panic.
    fn report(
        &self,
        cx: &mut App,
        task: impl FnOnce(&mut ClaudhubApp, &mut Context<ClaudhubApp>) + 'static,
    ) {
        let Some(app) = self.app.clone() else { return };
        cx.defer(move |cx| {
            app.update(cx, |this, cx| task(this, cx)).ok();
        });
    }
}

impl TableDelegate for Results {
    fn columns_count(&self, _: &App) -> usize {
        self.rows.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.rows.len()
    }

    fn column(&self, index: usize, _: &App) -> Column {
        let name = self
            .rows
            .columns
            .get(index)
            .cloned()
            .unwrap_or_else(|| index.to_string());
        let column = Column::new(name.clone(), name)
            .width(self.widths.get(index).copied().unwrap_or(px(120.)))
            .resizable(true)
            // The padding moves from the column to our elements: without that,
            // eight pixels on each side of a cell do not answer the click, and a
            // cell that has to be aimed at is not a cell one selects.
            .p_0();
        if !self.sortable {
            return column;
        }
        // The arrow is painted from here, and `refresh` re-reads this function
        // on every result: that is what brings the display back into line with
        // the sort actually sent.
        match self.sort {
            Some(sort) if sort.column == index && sort.ascending => column.ascending(),
            Some(sort) if sort.column == index => column.descending(),
            _ => column.sort(ColumnSort::Default),
        }
    }

    fn perform_sort(
        &mut self,
        index: usize,
        _: ColumnSort,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        let next = self.next_sort(index);
        let console = self.console;
        self.report(cx, move |this, cx| this.sort_db_query(console, next, cx));
    }

    /// The whole header is clickable, and not just its little arrow.
    ///
    /// It is DataGrip's and PhpStorm's gesture: one aims at the column's name.
    /// The arrow the table paints beside it stays the state's cue and triggers
    /// the same thing.
    fn render_th(
        &mut self,
        index: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let name = self
            .rows
            .columns
            .get(index)
            .cloned()
            .unwrap_or_else(|| index.to_string());
        let label = div()
            .size_full()
            .px_2()
            .flex()
            .items_center()
            .truncate()
            .child(SharedString::from(name));
        if !self.sortable {
            return label.into_any_element();
        }
        label
            .id(("db-th", index))
            .cursor_pointer()
            .on_click(cx.listener(move |table, _, _window, cx| {
                let next = table.delegate().next_sort(index);
                let console = table.delegate().console;
                table
                    .delegate()
                    .report(cx, move |this, cx| this.sort_db_query(console, next, cx));
            }))
            .into_any_element()
    }

    fn render_td(
        &mut self,
        row: usize,
        column: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let cell = self.rows.rows.get(row).and_then(|row| row.get(column));
        // `NULL` is a value and not a text: dimming it is what tells it from the
        // string "NULL" a column may contain, and the result really does carry
        // two different things.
        let (text, null) = match cell {
            Some(Some(value)) => (SharedString::from(value.clone()), false),
            _ => (SharedString::new_static("NULL"), true),
        };
        let selected = self
            .selection
            .is_some_and(|selection| selection.contains(row, column));
        // A foreign key is tinted, as it is in the schema tree, and that tint is
        // the whole of what says it can be followed: the gesture is a
        // system-key click, like going to a definition in the editor, and a
        // colour is what makes it discoverable without a hint on every row.
        let key = self.link(row, column).is_some();
        div()
            .size_full()
            .px_2()
            .flex()
            .items_center()
            .truncate()
            .when_some(self.mono.clone(), |el, mono| el.font_family(mono))
            .when(null, |el| {
                el.text_color(cx.theme().muted_foreground).italic()
            })
            .when(key, |el| el.text_color(cx.theme().info))
            // **The underline exists only while the key is held**, exactly as
            // it does on a diff line: it is the answer to "what does this click
            // do", asked by holding the modifier down, and a cell underlined at
            // rest would read as a link one clicks plainly. The hand cursor
            // comes with it, and the styling is the element's own hover — the
            // grid has no hovered cell of its own to keep, and one more piece
            // of state repainted per pointer move is what we would be paying
            // for a rule the compositor already applies.
            .when(key && self.armed, |el| {
                el.cursor_pointer().hover(|el| el.underline())
            })
            .when(selected, |el| el.bg(cx.theme().selection))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |table, event: &gpui::MouseDownEvent, window, cx| {
                    // A click in the grid **takes the focus**, like a click on a
                    // diff line: without that the `Ctrl+C` that follows goes to
                    // whoever had it — the terminal, the query editor — and the
                    // `ClaudhubQuery` context is not in the stack.
                    let focus = table.focus_handle(cx);
                    window.focus(&focus, cx);
                    table
                        .delegate_mut()
                        .press(row, column, event.modifiers.shift);
                    if event.modifiers.secondary() {
                        // Deferred **with the window**: we are inside an
                        // `update` on the table, and the jump replaces its
                        // delegate — the panic `report` already exists for. The
                        // window is needed all the same, the query being put
                        // into the editor's state.
                        if let Some(app) = table.delegate().app.clone() {
                            let console = table.delegate().console;
                            window.defer(cx, move |window, cx| {
                                app.update(cx, |this, cx| {
                                    this.follow_db_key(console, row, column, window, cx);
                                })
                                .ok();
                            });
                        }
                    }
                    cx.notify();
                }),
            )
            // The button is rechecked on every movement and not only on the
            // press: a release outside the window sends no event, and the
            // selection would follow the cursor afterwards.
            .on_mouse_move(
                cx.listener(move |table, event: &gpui::MouseMoveEvent, _window, cx| {
                    if !table.delegate().dragging {
                        return;
                    }
                    if event.pressed_button != Some(gpui::MouseButton::Left) {
                        table.delegate_mut().dragging = false;
                        return;
                    }
                    if table.delegate_mut().drag_to(row, column) {
                        cx.notify();
                    }
                }),
            )
            // A right click **outside** the selection replaces it; inside, it
            // keeps it — otherwise the "copy selection" menu would copy the one
            // cell just aimed at.
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(move |table, _event, _window, cx| {
                    let inside = table
                        .delegate()
                        .selection
                        .is_some_and(|selection| selection.contains(row, column));
                    if !inside {
                        let delegate = table.delegate_mut();
                        delegate.press(row, column, false);
                        delegate.dragging = false;
                        cx.notify();
                    }
                }),
            )
            .child(text)
    }

    /// The right-click menu.
    ///
    /// It carries what one does with a result being looked at: copy what has
    /// been selected, the whole row, everything, or write it to a file. Every
    /// entry carries an icon — it is the convention of all Claudhub's menus, and
    /// a single entry without one shifts the others.
    fn context_menu(
        &mut self,
        row: usize,
        menu: gpui_component::menu::PopupMenu,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> gpui_component::menu::PopupMenu {
        let Some(app) = self.app.clone() else {
            return menu;
        };
        let console = self.console;
        let selected = self.selection.is_some();
        // The cell aimed at is the selection's cursor: a right click outside the
        // selection has just put it there, and inside a block it is the last
        // cell clicked. A jump is about one value, never about a rectangle.
        let jump = self
            .selection
            .map(|selection| selection.cursor)
            .filter(|(clicked, _)| *clicked == row)
            .and_then(|(row, column)| {
                let (target, value) = self.link(row, column)?;
                Some((
                    target.label(),
                    db::link::select_row(self.engine, target, value),
                ))
            });
        let (copy, headers, line, all, export) = (
            app.clone(),
            app.clone(),
            app.clone(),
            app.clone(),
            app.clone(),
        );
        let menu = match jump {
            Some((label, sql)) => menu
                .item(
                    PopupMenuItem::new(tr!("db-follow-key", { target: label }))
                        .icon(icon("arrow-right"))
                        .on_click(move |_, window, cx| {
                            app.update(cx, |this, cx| {
                                this.run_db_sql(console, sql.clone(), window, cx)
                            })
                            .ok();
                        }),
                )
                .separator(),
            None => menu,
        };
        menu.item(
            PopupMenuItem::new(tr!("db-copy-selection"))
                .icon(icon("copy"))
                .disabled(!selected)
                .on_click(move |_, _window, cx| {
                    copy.update(cx, |this, cx| this.copy_db_selection(console, false, cx))
                        .ok();
                }),
        )
        .item(
            PopupMenuItem::new(tr!("db-copy-with-headers"))
                .icon(icon("table"))
                .disabled(!selected)
                .on_click(move |_, _window, cx| {
                    headers
                        .update(cx, |this, cx| this.copy_db_selection(console, true, cx))
                        .ok();
                }),
        )
        .separator()
        .item(
            PopupMenuItem::new(tr!("db-copy-row"))
                .icon(icon("copy"))
                .on_click(move |_, _window, cx| {
                    line.update(cx, |this, cx| this.copy_db_row(console, row, cx))
                        .ok();
                }),
        )
        .item(
            PopupMenuItem::new(tr!("db-copy-result"))
                .icon(icon("copy"))
                .on_click(move |_, _window, cx| {
                    all.update(cx, |this, cx| this.copy_db_all(console, cx))
                        .ok();
                }),
        )
        .separator()
        .item(
            PopupMenuItem::new(tr!("db-export"))
                .icon(icon("download"))
                .on_click(move |_, _window, cx| {
                    export
                        .update(cx, |this, cx| this.export_db_csv(console, cx))
                        .ok();
                }),
        )
    }

    fn cell_text(&self, row: usize, column: usize, _: &App) -> String {
        self.rows
            .rows
            .get(row)
            .and_then(|row| row.get(column))
            .cloned()
            .flatten()
            .unwrap_or_default()
    }

    /// Scrolling can ask for more as long as there is some left.
    fn has_more(&self, _: &App) -> bool {
        self.more && !self.loading
    }

    fn load_more(&mut self, _: &mut Window, cx: &mut Context<TableState<Self>>) {
        if self.loading {
            return;
        }
        self.loading = true;
        let console = self.console;
        self.report(cx, move |this, cx| this.extend_db_rows(console, cx));
    }
}

impl ClaudhubApp {
    // — The consoles the window holds ————————————————————————————

    /// The console this gesture is about, when it is still open.
    pub(super) fn console(&self, id: ConsoleId) -> Option<&Console> {
        self.consoles.iter().find(|console| console.id == id)
    }

    pub(super) fn console_mut(&mut self, id: ConsoleId) -> Option<&mut Console> {
        self.consoles.iter_mut().find(|console| console.id == id)
    }

    /// The console a keystroke is about: the last one to have held the focus,
    /// if it is still open.
    ///
    /// A gesture that comes from a console's own bar carries its id and never
    /// asks this. What asks are the bindings — `Ctrl+Enter`, `Ctrl+C` on the
    /// grid — which have only the focus to go on.
    pub(super) fn focused_console(&self) -> Option<ConsoleId> {
        let id = self.active_console?;
        self.console(id).map(|console| console.id)
    }

    /// The consoles of one worktree, in the order they were opened.
    pub(super) fn consoles_of<'a>(
        &'a self,
        worktree: &'a std::path::Path,
    ) -> impl Iterator<Item = &'a Console> + 'a {
        self.consoles
            .iter()
            .filter(move |console| console.worktree == worktree)
    }

    /// Whether a console is open on the worktree being looked at.
    ///
    /// What the status bar and the session ask. Not what decides a tab any
    /// more: a console **is** a tab now, so it is there exactly as long as it
    /// is open.
    pub(super) fn db_console_open(&self) -> bool {
        self.active
            .as_deref()
            .is_some_and(|worktree| self.consoles_of(worktree).next().is_some())
    }

    /// The lowest rank no open console holds.
    ///
    /// The **lowest free** and not the next of a counter: closing `SQL 1` and
    /// opening another must give `SQL 1` back, or a long session ends up with
    /// `SQL 12` beside `SQL 27`. Ranks are per window and not per worktree —
    /// two tabs of the centre showing "SQL 1" would name the same thing twice.
    fn free_console_rank(&self) -> usize {
        (1..)
            .find(|rank| !self.consoles.iter().any(|console| console.rank == *rank))
            .unwrap_or(1)
    }

    /// Opens a console on a worktree, and gives back what names it.
    ///
    /// Everything a console needs is built here, which is the gpui rule said
    /// once per console rather than once per window: an `EditorState` rebuilt
    /// at render time loses the cursor, the selection and the text on the first
    /// keystroke, and a `TableState` rebuilt loses the column widths.
    ///
    /// `asked_for` tells a gesture from a restoration: a console one has just
    /// asked for takes the tab and the keyboard, one being put back by the
    /// session takes neither — the document in front is the checkout's own, put
    /// back a step earlier, and a console landing over it would be the window
    /// choosing where one was.
    pub(super) fn open_console(
        &mut self,
        worktree: std::path::PathBuf,
        asked_for: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ConsoleId {
        self.console_seq += 1;
        let id = ConsoleId(self.console_seq);
        let rank = self.free_console_rank();
        let schema: Rc<RefCell<SchemaIndex>> = Default::default();
        let input = cx.new(|cx| {
            gpui_component::input::EditorState::new(window, cx)
                .language("sql")
                .line_number(true)
                .placeholder("SELECT * FROM …")
        });
        input.update(cx, |state, cx| {
            state.lsp_mut().completion_provider = Some(Rc::new(SqlCompletions {
                schema: schema.clone(),
            }));
            cx.notify();
        });
        // The modal harness, created with the editor and for its whole life:
        // the decoration layers follow the text through its edits.
        let host = crate::ui::surface::VimHost::new(&input, cx);
        let table = cx.new(|cx| {
            TableState::new(Results::default(), window, cx)
                // The headers carry their sort arrow. gpui-component's cell
                // selection, for its part, stays off: it knows only one at a
                // time, and what one copies from a result grid is almost always
                // a block — see `db_query::Results::selection`.
                .sortable(true)
        });
        // The query being written is part of where one was, so it is filed as
        // it is typed — on `Change` and not on sending, because what one comes
        // back to tomorrow is often precisely the query that was never sent.
        // The store's write is deferred by half a second, which is what keeps a
        // keystroke from costing a file.
        cx.subscribe(&input, |this, _, event, cx| {
            if matches!(event, gpui_component::input::InputEvent::Change) {
                this.persist_session(cx);
            }
        })
        .detach();
        let app = cx.entity();
        // Given and not read: the panel is built inside this `update`, so it
        // cannot read the application to find out for itself.
        let title = console_title(rank);
        let visible = self.active.as_deref() == Some(worktree.as_path());
        let panel = {
            let (input, title, worktree) = (input.clone(), title, worktree.clone());
            cx.new(|cx| {
                crate::ui::panels::QueryPanel::new(&app, id, input, title, worktree, visible, cx)
            })
        };
        self.consoles.push(Console {
            id,
            rank,
            worktree,
            state: QueryState::default(),
            input,
            host,
            schema,
            table,
            split: split_state(cx),
            panel: panel.clone(),
        });
        if asked_for {
            self.active_console = Some(id);
        }
        self.dock_console(panel, asked_for, window, cx);
        id
    }

    /// Puts a console's panel among the documents, beside the ones already
    /// there.
    ///
    /// The centre, always: a console is what one reads, and the left picks
    /// while the right remembers. A console already open is the group it joins,
    /// which is what makes the dock's bar read as the row of consoles.
    fn dock_console(
        &mut self,
        panel: Entity<crate::ui::panels::QueryPanel>,
        asked_for: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::dock::{DockPlacement, InsertTarget, PanelId};
        let dock = self.dock.clone();
        let sibling = self
            .consoles
            .iter()
            .map(|console| console.panel.clone())
            .rfind(|other| other.entity_id() != panel.entity_id());
        dock.update(cx, |dock, cx| {
            // **`panel_handle` and `dock_panel_at`, never `add_panel`.** An
            // `Entity<P>` converts itself into base's `PanelView` and the dock
            // takes it without complaint — but without the presentation that
            // goes with it: no tab, no title, no content.
            crate::ui::panels::dock_panel_at(
                dock,
                gpui_component::dock::panel_handle(panel.clone()),
                DockPlacement::Center,
                None,
                |dock| {
                    let sibling = sibling?;
                    let node = dock
                        .layout(DockPlacement::Center)?
                        .find_panel_node(PanelId::from(sibling.entity_id()))?;
                    Some(InsertTarget::Tabs {
                        node,
                        ix: None,
                        // The tab one has just opened is the tab one looks at.
                        activate: asked_for,
                    })
                },
                window,
                cx,
            );
        });
        if asked_for {
            let handle = gpui::Focusable::focus_handle(&panel, cx);
            window.focus(&handle, cx);
        }
        cx.notify();
    }

    /// Closes a console: the tab goes, and so does everything it held.
    ///
    /// The gesture of the cross on the tab, of the bar's own `×`, and of a
    /// worktree going away. What the panel's `on_removed` calls is
    /// `console_gone`: by then the dock has taken the tab out itself, and
    /// asking it again would be asking it to remove what it is removing.
    pub(super) fn close_console(
        &mut self,
        id: ConsoleId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.consoles.iter().position(|console| console.id == id) else {
            return;
        };
        let panel = self.consoles[ix].panel.clone();
        let dock = self.dock.clone();
        dock.update(cx, |dock, cx| dock.remove_panel(panel, window, cx));
        self.console_gone(id, window, cx);
    }

    /// Forgets a console whose tab has already gone.
    ///
    /// The focus was on what has just left the tree, and a focus handle nobody
    /// renders any more resolves no binding: every shortcut would stay dead
    /// until a click put the focus back on a live node.
    pub(super) fn console_gone(
        &mut self,
        id: ConsoleId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.consoles.iter().position(|console| console.id == id) else {
            return;
        };
        let console = self.consoles.remove(ix);
        let had_focus = gpui::Focusable::focus_handle(&console.panel, cx).is_focused(window);
        if self.active_console == Some(id) {
            // The neighbour, which is what one is left looking at.
            self.active_console = self.consoles.last().map(|console| console.id);
        }
        if had_focus {
            match self.active_console.and_then(|id| self.console(id)) {
                Some(console) => {
                    let handle = gpui::Focusable::focus_handle(&console.panel, cx);
                    window.focus(&handle, cx);
                }
                None => {
                    let root = self.focus.clone();
                    window.focus(&root, cx);
                }
            }
        }
        self.persist_session(cx);
        cx.notify();
    }

    /// Drops every console of a worktree — the worktree is gone.
    pub(super) fn close_consoles_of(
        &mut self,
        worktree: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let doomed: Vec<ConsoleId> = self
            .consoles_of(worktree)
            .map(|console| console.id)
            .collect();
        for id in doomed {
            self.close_console(id, window, cx);
        }
    }

    /// Notes which console a binding is about, from the frame that paints one.
    ///
    /// **The focus and not the painting.** Two consoles can be drawn side by
    /// side in a split, so "the last one painted" alternates between them while
    /// nothing is being clicked — the trap `Surface::File` exists for. A
    /// console that holds the keyboard is the one `Ctrl+Enter` means; the last
    /// one that did is the one a click in the schema tree reuses.
    pub(super) fn console_focused(
        &mut self,
        id: ConsoleId,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(console) = self.console(id) else {
            return;
        };
        let focused = gpui::Focusable::focus_handle(&console.input, cx).is_focused(window)
            || gpui::Focusable::focus_handle(&console.table, cx).is_focused(window);
        if focused {
            self.active_console = Some(id);
        }
    }

    /// Brings a console's tab forward, and gives it the keyboard.
    pub(super) fn reveal_console(
        &mut self,
        id: ConsoleId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(console) = self.console(id) else {
            return;
        };
        let panel = console.panel.clone();
        crate::ui::panels::QueryPanel::activate(&panel, window, cx);
        let handle = gpui::Focusable::focus_handle(&panel, cx);
        window.focus(&handle, cx);
        self.active_console = Some(id);
    }

    // — Opening one on a table ————————————————————————————————————

    /// Opens a connection, and possibly a table, in a console.
    ///
    /// `target` is the whole of the difference between the two gestures: a
    /// click in the schema tree **reuses** the console one is in, because
    /// browsing a schema with the mouse would otherwise leave ten tabs behind;
    /// the `+`, `Ctrl+Shift+Q` and "open in a new tab" ask for one of their own.
    pub(super) fn start_db_console(
        &mut self,
        target: ConsoleTarget,
        connection: db::Connection,
        database: Option<String>,
        table: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Read before anything moves: opening a table replaces the query one
        // was reading, and that is the thing which had nowhere to be written.
        let from = self.here(cx);
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let id = match target {
            ConsoleTarget::Current => match self.focused_console() {
                Some(id) => id,
                // Nothing to reuse: the gesture opens the first one.
                None => self.open_console(worktree, true, window, cx),
            },
            ConsoleTarget::NewTab => self.open_console(worktree, true, window, cx),
        };
        let Some(console) = self.console_mut(id) else {
            return;
        };
        let changed = console.state.connection.as_ref() != Some(&connection)
            || console.state.database != database;
        console.state.connection = Some(connection.clone());
        console.state.database = database.clone();
        if changed {
            console.state.error = None;
            console.state.sent = None;
            console.state.sort = None;
            console.state.can_sort = false;
            self.set_db_rows(id, db::Rows::default(), cx);
            self.index_db_schema(id, &connection, database.as_deref(), cx);
        }
        if let Some(table) = table {
            let quoted = match connection.engine {
                db::Engine::Sqlite => format!("\"{table}\""),
                db::Engine::Mysql => format!("`{table}`"),
            };
            // No `LIMIT`: the result window already stands for one, and a bound
            // written into the text would outlive the query one writes over it.
            let sql = format!("SELECT * FROM {quoted};");
            if let Some(console) = self.console(id) {
                let input = console.input.clone();
                input.update(cx, |state, cx| {
                    state.set_value(sql.clone(), window, cx);
                });
            }
            self.run_db_query(id, cx);
            self.record_step(
                from,
                crate::ui::jumps::Place::Query {
                    connection: connection.key(),
                    database: database.clone(),
                    sql,
                },
                cx,
            );
        }
        // A console opened with no table is **not** a step of the trail: there
        // is nothing in it to come back to, and a back arrow landing on an
        // empty editor is a step that undoes nothing one can see.
        //
        // Opening a console brings its tab forward: the gesture comes from the
        // schema tree, but also from the menu of a table opened from somewhere
        // else entirely.
        self.reveal_console(id, window, cx);
        self.persist_session(cx);
        cx.notify();
    }

    /// Puts a console back where the previous session left it.
    ///
    /// Everything `start_db_console` does apart from its side effects: it
    /// neither calls up anything nor takes the focus. Restoring is not a
    /// gesture.
    ///
    /// The query is put in the editor and **not sent**: what one comes back to
    /// is the text one was writing, and replaying a `SELECT` nobody asked for
    /// is a query against a server one did not ask to reach.
    pub(super) fn reopen_db_console(
        &mut self,
        worktree: std::path::PathBuf,
        connection: db::Connection,
        database: Option<String>,
        query: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ConsoleId {
        let id = self.open_console(worktree, false, window, cx);
        if let Some(console) = self.console_mut(id) {
            console.state.connection = Some(connection.clone());
            console.state.database = database.clone();
            let input = console.input.clone();
            input.update(cx, |state, cx| {
                state.set_value(query, window, cx);
            });
        }
        self.index_db_schema(id, &connection, database.as_deref(), cx);
        cx.notify();
        id
    }

    /// Asks for the names one console will complete.
    ///
    /// It is the same command as the panel's: if the tree has already indexed
    /// this database, the answer fills both.
    fn index_db_schema(
        &mut self,
        id: ConsoleId,
        connection: &db::Connection,
        database: Option<&str>,
        _cx: &mut Context<Self>,
    ) {
        let database = match (connection.engine, database) {
            (db::Engine::Sqlite, _) => "main".to_string(),
            (db::Engine::Mysql, Some(name)) => name.to_string(),
            // With no database chosen, there is no schema to index: the
            // completions are limited to the keywords.
            (db::Engine::Mysql, None) => return,
        };
        if let Some(console) = self.console(id) {
            console.schema.borrow_mut().database = None;
        }
        self.git.send(Cmd::DbAllColumns {
            connection: connection.clone(),
            database,
        });
    }

    /// Files a schema that has just arrived, in **every** console waiting for
    /// it.
    ///
    /// Every one and not the first: two consoles open on the same database ask
    /// the same question, and one answer fills both — a second command would be
    /// a second round trip for a schema already in hand.
    pub(super) fn db_schema_indexed(
        &mut self,
        key: &str,
        database: &str,
        columns: &BTreeMap<String, Vec<db::Column>>,
        cx: &mut Context<Self>,
    ) {
        let waiting: Vec<ConsoleId> = self
            .consoles
            .iter()
            .filter(|console| {
                let Some(connection) = console.state.connection.as_ref() else {
                    return false;
                };
                if connection.key() != key {
                    return false;
                }
                let expected = match connection.engine {
                    db::Engine::Sqlite => "main",
                    db::Engine::Mysql => console.state.database.as_deref().unwrap_or_default(),
                };
                expected == database
            })
            .map(|console| console.id)
            .collect();
        for id in waiting {
            let Some(console) = self.console(id) else {
                continue;
            };
            {
                let mut index = console.schema.borrow_mut();
                index.database = Some(database.to_string());
                index.tables = columns
                    .iter()
                    .map(|(table, columns)| {
                        (
                            table.clone(),
                            columns.iter().map(|column| column.name.clone()).collect(),
                        )
                    })
                    .collect();
                index.foreign_keys = db::link::keys_of(columns);
            }
            // A result shown before the index arrived carries no key yet. The
            // links are recomputed rather than the table refreshed: `refresh`
            // would put the scrolling back to the top, and what changes here is
            // only what the cells are painted with.
            let table = console.table.clone();
            let shown = table.read(cx).delegate().rows.columns.clone();
            let links = self.db_links(id, &shown);
            table.update(cx, |state, cx| {
                state.delegate_mut().links = links;
                cx.notify();
            });
        }
        cx.notify();
    }

    // — The gestures ——————————————————————————————————————————————

    /// Tells every result grid whether the system key is held.
    ///
    /// Pushed and not read: see `Results::armed`. It costs a frame at each flip
    /// of the modifier, and only when a console is open.
    pub(super) fn arm_db_follow(&mut self, armed: bool, cx: &mut Context<Self>) {
        let tables: Vec<_> = self
            .consoles
            .iter()
            .map(|console| console.table.clone())
            .collect();
        for table in tables {
            table.update(cx, |state, cx| {
                if state.delegate().armed != armed {
                    state.delegate_mut().armed = armed;
                    cx.notify();
                }
            });
        }
    }

    /// Follows the foreign key a cell carries.
    ///
    /// The gesture is the system-key click and the menu entry, and both end in
    /// `run_db_sql` — two ways of making one gesture that did not land in the
    /// same place would be one too many.
    pub(super) fn follow_db_key(
        &mut self,
        id: ConsoleId,
        row: usize,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(console) = self.console(id) else {
            return;
        };
        let results = console.table.read(cx).delegate();
        let sql = results
            .link(row, column)
            .map(|(target, value)| db::link::select_row(results.engine, target, value));
        if let Some(sql) = sql {
            self.run_db_sql(id, sql, window, cx);
        }
    }

    /// Puts a query into a console's editor and runs it.
    ///
    /// It is what opening a table from the tree does, and following a key does
    /// the same: the text is what one goes on to adjust, and the previous query
    /// is one row up in the history panel — which is what makes this
    /// overwriting bearable.
    pub(super) fn run_db_sql(
        &mut self,
        id: ConsoleId,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let from = self.here(cx);
        let Some(console) = self.console(id) else {
            return;
        };
        let input = console.input.clone();
        input.update(cx, |state, cx| {
            state.set_value(sql.clone(), window, cx);
        });
        self.run_db_query(id, cx);
        let Some(console) = self.console(id) else {
            return;
        };
        if let Some(connection) = console.state.connection.as_ref() {
            let to = crate::ui::jumps::Place::Query {
                connection: connection.key(),
                database: console.state.database.clone(),
                sql,
            };
            self.record_step(from, to, cx);
        }
    }

    /// Puts a query of the trail back, and runs it.
    ///
    /// It runs, where restoring a session does not: a step back is a gesture,
    /// asked for now, and what one is coming back to is the **result** — the
    /// row a foreign key was followed from.
    ///
    /// It goes into the console one is in, and opens one where there is none: a
    /// step back must land somewhere, and a tab of its own for every step would
    /// turn the back arrow into a way of filling the centre.
    ///
    /// It is **not filed in the history**: the query is already there, one row
    /// up, and a `×2` counts the times a question was asked and not the times
    /// one walked back past it.
    pub(super) fn replay_db_query(
        &mut self,
        connection: String,
        database: Option<String>,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Named by its key, as a session names it: a connection deleted in the
        // meantime simply has nowhere to go back to.
        let Some(target) = crate::ui::settings::Settings::global(cx)
            .databases
            .iter()
            .find(|candidate| candidate.key() == connection)
            .cloned()
        else {
            return;
        };
        let Some(worktree) = self.active.clone() else {
            return;
        };
        let here = self.focused_console();
        let elsewhere = here.and_then(|id| self.console(id)).is_none_or(|console| {
            console.state.connection.as_ref().map(|c| c.key()) != Some(connection)
                || console.state.database != database
        });
        let id = match (here, elsewhere) {
            (Some(id), false) => id,
            _ => self.reopen_db_console(worktree, target, database, String::new(), window, cx),
        };
        self.reveal_console(id, window, cx);
        if let Some(console) = self.console(id) {
            let input = console.input.clone();
            input.update(cx, |state, cx| {
                state.set_value(sql, window, cx);
            });
        }
        self.run_db_query(id, cx);
        if let Some(console) = self.console_mut(id) {
            console.state.record = false;
        }
        self.persist_session(cx);
        cx.notify();
    }

    /// Runs whatever is in a console's editor.
    ///
    /// The sort starts again from scratch: it is about a column of the result,
    /// and nothing says the new query has the same one.
    pub(super) fn run_db_query(&mut self, id: ConsoleId, cx: &mut Context<Self>) {
        let Some(console) = self.console(id) else {
            return;
        };
        let sql = console.input.read(cx).value().to_string();
        if sql.trim().is_empty() {
            return;
        }
        let Some(console) = self.console_mut(id) else {
            return;
        };
        console.state.sent = Some(sql);
        console.state.sort = None;
        console.state.can_sort = false;
        console.state.record = true;
        self.send_db_query(id, 0, false, cx);
    }

    /// Sorts a result, or removes its sort.
    ///
    /// The window goes back to its start: the rows that filled it are no longer
    /// the first of anything.
    pub(super) fn sort_db_query(
        &mut self,
        id: ConsoleId,
        sort: Option<Sort>,
        cx: &mut Context<Self>,
    ) {
        let Some(console) = self.console_mut(id) else {
            return;
        };
        if !console.state.can_sort || console.state.sort == sort {
            return;
        }
        console.state.sort = sort;
        console.state.record = false;
        // The arrow follows the gesture and not the answer: a query sometimes
        // takes a second, and a header that does not move reads as a lost click.
        let table = console.table.clone();
        table.update(cx, |state, cx| {
            state.delegate_mut().sort = sort;
            state.refresh(cx);
        });
        self.send_db_query(id, 0, false, cx);
    }

    /// Moves the window.
    pub(super) fn page_db_query(&mut self, id: ConsoleId, offset: usize, cx: &mut Context<Self>) {
        if let Some(console) = self.console_mut(id) {
            console.state.record = false;
        }
        self.send_db_query(id, offset, false, cx);
    }

    /// Extends the window: scrolling has reached the bottom.
    pub(super) fn extend_db_rows(&mut self, id: ConsoleId, cx: &mut Context<Self>) {
        let Some(console) = self.console_mut(id) else {
            return;
        };
        if console.state.running || !console.state.more {
            // The table put itself in a waiting state before calling us; without
            // this, it would never come out of it.
            let table = console.table.clone();
            table.update(cx, |state, _| {
                state.delegate_mut().loading = false;
            });
            return;
        }
        console.state.record = false;
        let next = console.state.offset + console.state.shown;
        self.send_db_query(id, next, true, cx);
    }

    /// The query as it really goes out: the one that was run, and the sort asked
    /// for around it.
    fn effective_sql(&self, id: ConsoleId) -> Option<String> {
        let state = &self.console(id)?.state;
        let sent = state.sent.clone()?;
        match state.sort {
            Some(sort) => Some(db::order_by(&sent, sort.column, sort.ascending).unwrap_or(sent)),
            None => Some(sent),
        }
    }

    fn send_db_query(
        &mut self,
        id: ConsoleId,
        offset: usize,
        append: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(sql) = self.effective_sql(id) else {
            return;
        };
        let Some(console) = self.console(id) else {
            return;
        };
        let Some(connection) = console.state.connection.clone() else {
            return;
        };
        let limit = Settings::global(cx).db_page_size.max(1);
        // Counted for the **window**: it is the only thing the answer carries
        // back, and two consoles counting from one would each take the other's
        // rows.
        self.db_request_seq += 1;
        let request = self.db_request_seq;
        let Some(console) = self.console_mut(id) else {
            return;
        };
        console.state.request = request;
        console.state.appending = append;
        console.state.running = true;
        console.state.error = None;
        let database = console.state.database.clone();
        self.git.send(Cmd::DbQuery {
            connection,
            database,
            sql,
            offset,
            limit,
            request,
        });
        cx.notify();
    }

    /// A query's result, for whichever console is still waiting for it.
    ///
    /// It is **dropped if it does not answer that console's last request**: one
    /// restarts before the previous has come back — by changing page, by
    /// sorting, by scrolling down — and showing the late answer would replace
    /// what is being looked at with what is not.
    pub(super) fn db_rows_arrived(
        &mut self,
        request: u64,
        rows: crate::runtime::protocol::DbResult<db::Rows>,
        elapsed_ms: u64,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self
            .consoles
            .iter()
            .find(|console| console.state.request == request)
            .map(|console| console.id)
        else {
            return;
        };
        let Some(console) = self.console_mut(id) else {
            return;
        };
        console.state.running = false;
        console.state.elapsed_ms = elapsed_ms;
        // Filed in the history, and only what a gesture asked for — see
        // `QueryState::record`.
        if std::mem::take(&mut console.state.record) {
            self.record_sql_query(id, &rows, elapsed_ms, cx);
        }
        match rows {
            Ok(rows) => {
                let Some(console) = self.console_mut(id) else {
                    return;
                };
                console.state.error = None;
                let sent = console.state.sent.clone().unwrap_or_default();
                console.state.can_sort = db::can_order(&sent, &rows.columns);
                console.state.affected = rows.affected;
                if console.state.appending {
                    console.state.more = rows.more;
                    console.state.shown += rows.rows.len();
                    self.extend_db_table(id, rows, cx);
                } else {
                    console.state.offset = rows.offset;
                    console.state.shown = rows.rows.len();
                    console.state.more = rows.more;
                    console.state.has_columns = !rows.columns.is_empty();
                    self.set_db_rows(id, rows, cx);
                }
            }
            Err(message) => {
                let Some(console) = self.console_mut(id) else {
                    return;
                };
                console.state.error = Some(message.into());
                console.state.has_columns = false;
                console.state.can_sort = false;
                console.state.more = false;
                self.set_db_rows(id, db::Rows::default(), cx);
            }
        }
        cx.notify();
    }

    /// Replaces a console's table content.
    ///
    /// The table is an entity created once: rebuilding it on every result would
    /// lose the widths just adjusted with the mouse and would put the scrolling
    /// back to the top in the middle of paging.
    fn set_db_rows(&mut self, id: ConsoleId, rows: db::Rows, cx: &mut Context<Self>) {
        let links = self.db_links(id, &rows.columns);
        let armed = self.follow_armed;
        let Some(console) = self.console(id) else {
            return;
        };
        let mut results = Results::new(id, rows, links, &console.state, cx);
        // A result that lands while the key is held is followable straight
        // away: the flag is only pushed when the modifier *flips*, and paging
        // with `Ctrl` down would otherwise paint a grid that says nothing can
        // be followed until one lets go of it.
        results.armed = armed;
        let table = console.table.clone();
        table.update(cx, |state, cx| {
            *state.delegate_mut() = results;
            state.refresh(cx);
        });
    }

    /// Which of a result's columns can be followed.
    ///
    /// Empty as long as the schema has not been indexed — the answer comes
    /// several seconds after the console opens, and `db_schema_indexed` asks
    /// again then.
    fn db_links(&self, id: ConsoleId, columns: &[String]) -> Vec<Option<db::link::Target>> {
        let Some(console) = self.console(id) else {
            return vec![None; columns.len()];
        };
        let Some(sql) = console.state.sent.as_deref() else {
            return vec![None; columns.len()];
        };
        let index = console.schema.borrow();
        db::link::targets(sql, columns, &index.foreign_keys)
    }

    /// Appends a page under the ones being looked at.
    ///
    /// The widths are **not** recomputed: they were derived from the first page,
    /// and revisiting them on every extension would move the columns under the
    /// eyes of whoever is scrolling. `refresh` is not called either — it would
    /// put the scrolling back to the top, which is exactly the opposite of what
    /// was just asked for.
    fn extend_db_table(&mut self, id: ConsoleId, rows: db::Rows, cx: &mut Context<Self>) {
        let Some(console) = self.console(id) else {
            return;
        };
        let table = console.table.clone();
        table.update(cx, |state, cx| {
            let delegate = state.delegate_mut();
            delegate.rows.extend(rows);
            delegate.more = delegate.rows.more;
            delegate.loading = false;
            cx.notify();
        });
    }

    /// Selects the whole loaded result.
    pub(super) fn select_whole_db_result(&mut self, id: ConsoleId, cx: &mut Context<Self>) {
        let Some(console) = self.console(id) else {
            return;
        };
        let table = console.table.clone();
        table.update(cx, |state, cx| {
            state.delegate_mut().select_all();
            cx.notify();
        });
    }

    /// Copies what is selected.
    ///
    /// A null cell copies **nothing** and not the word "NULL": that word is how
    /// the grid shows the absence of a value, and it means nothing once pasted
    /// elsewhere.
    pub(super) fn copy_db_selection(
        &mut self,
        id: ConsoleId,
        headers: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(console) = self.console(id) else {
            return;
        };
        let Some(text) = console.table.read(cx).delegate().selected_text(headers) else {
            return;
        };
        self.put_on_clipboard(text, cx);
    }

    /// Copies a whole row, with the column names above it.
    pub(super) fn copy_db_row(&mut self, id: ConsoleId, row: usize, cx: &mut Context<Self>) {
        let Some(console) = self.console(id) else {
            return;
        };
        let Some(text) = console.table.read(cx).delegate().row_text(row) else {
            return;
        };
        self.put_on_clipboard(text, cx);
    }

    /// Copies the whole **loaded** result — not the query's whole result, which
    /// is what the export writes.
    pub(super) fn copy_db_all(&mut self, id: ConsoleId, cx: &mut Context<Self>) {
        let Some(console) = self.console(id) else {
            return;
        };
        let table = console.table.read(cx);
        if table.delegate().rows.columns.is_empty() {
            return;
        }
        let text = table.delegate().all_text();
        self.put_on_clipboard(text, cx);
    }

    /// `Ctrl+C`: the selection if there is one, everything otherwise.
    ///
    /// Copying everything for want of a selection is already what the diff view
    /// does: on a result grid the gesture has no other meaning, and refusing to
    /// act would be a polite refusal for no reason.
    pub(super) fn copy_db_result(&mut self, id: ConsoleId, cx: &mut Context<Self>) {
        let selected = self
            .console(id)
            .is_some_and(|console| console.table.read(cx).delegate().selection.is_some());
        if selected {
            self.copy_db_selection(id, false, cx);
        } else {
            self.copy_db_all(id, cx);
        }
    }

    fn put_on_clipboard(&mut self, text: String, cx: &mut Context<Self>) {
        let lines = text.lines().count();
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.announce(tr!("db-copied", { n: lines }), cx);
    }

    /// Asks where to write, then starts the export.
    ///
    /// The native picker is asynchronous, hence the `spawn`: it is the same path
    /// as opening a repository.
    pub(super) fn export_db_csv(&mut self, id: ConsoleId, cx: &mut Context<Self>) {
        let Some(console) = self.console(id) else {
            return;
        };
        if console.state.exporting.is_some() || console.state.sent.is_none() {
            return;
        }
        let directory = directories::UserDirs::new()
            .map(|dirs| dirs.home_dir().to_path_buf())
            .unwrap_or_else(std::env::temp_dir);
        let name = console
            .state
            .connection
            .as_ref()
            .map(|connection| format!("{}.csv", connection.label()))
            .unwrap_or_else(|| "export.csv".to_string());
        let path = cx.prompt_for_new_path(&directory, Some(&name));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = path.await else {
                return; // cancelled
            };
            let _ = this.update(cx, |this, cx| this.send_db_export(id, path, cx));
        })
        .detach();
    }

    fn send_db_export(&mut self, id: ConsoleId, path: std::path::PathBuf, cx: &mut Context<Self>) {
        let Some(sql) = self.effective_sql(id) else {
            return;
        };
        let Some(console) = self.console(id) else {
            return;
        };
        let Some(connection) = console.state.connection.clone() else {
            return;
        };
        let database = console.state.database.clone();
        // The file is chosen here and written by the worker: on Windows, this is
        // therefore one of the few places where a path enters from this world
        // and has to come out in the server's. A folder the distribution cannot
        // reach — a network share — is refused rather than exported nowhere.
        let path = if cfg!(windows) {
            match crate::wslpath::for_server(&path) {
                Some(path) => path,
                None => {
                    self.announce(tr!("db-export-unreachable"), cx);
                    return;
                }
            }
        } else {
            path
        };
        if let Some(console) = self.console_mut(id) {
            console.state.exporting = Some(path.clone());
        }
        self.git.send(Cmd::DbExport {
            connection,
            database,
            sql,
            path,
        });
        cx.notify();
    }

    /// An export has come back. The path is given in full: it is the only thing
    /// one needs to remember to find it again — and it is also what says which
    /// console asked, the wire carrying nothing else back.
    pub(super) fn db_exported(
        &mut self,
        path: std::path::PathBuf,
        rows: crate::runtime::protocol::DbResult<u64>,
        cx: &mut Context<Self>,
    ) {
        for console in &mut self.consoles {
            if console.state.exporting.as_deref() == Some(path.as_path()) {
                console.state.exporting = None;
            }
        }
        match rows {
            Ok(count) => {
                // The server returns the path it wrote, so a Linux path: we give
                // it back to the user in the world they chose it in, otherwise
                // they would read `/mnt/c/…` of a file they will go looking for
                // in their explorer.
                let path = if cfg!(windows) {
                    let distro = crate::ui::settings::Settings::global(cx).wsl_distro.clone();
                    crate::wslpath::to_windows(&path, &distro)
                } else {
                    path
                };
                let file = SharedString::from(path.display().to_string());
                self.announce(tr!("db-exported", { n: count, path: file }), cx);
            }
            Err(message) => {
                self.announce_error(SharedString::from(message), cx);
            }
        }
        cx.notify();
    }

    // — What a binding means, with no console named ————————————————

    /// The console a binding is about, or nothing to do.
    ///
    /// Four bindings live on the query context — running, copying, selecting
    /// the grid, exporting — and that context is a console's own, so there is
    /// one and it has the focus. It is looked up rather than carried because a
    /// binding is dispatched by the window and knows no panel.
    pub(super) fn run_focused_db_query(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.focused_console() {
            self.run_db_query(id, cx);
        }
    }

    pub(super) fn copy_focused_db_result(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.focused_console() {
            self.copy_db_result(id, cx);
        }
    }

    pub(super) fn select_whole_focused_db_result(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.focused_console() {
            self.select_whole_db_result(id, cx);
        }
    }

    pub(super) fn export_focused_db_csv(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.focused_console() {
            self.export_db_csv(id, cx);
        }
    }

    /// `Ctrl+Shift+Q`: one more console, on what the current one is open on.
    ///
    /// It carries the connection over: asking for a second console is almost
    /// always asking for a second query against the same database, and making
    /// one pick the connection again would be a gesture undone by hand every
    /// time.
    pub(super) fn open_another_console(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let here = self
            .focused_console()
            .and_then(|id| self.console(id))
            .map(|console| {
                (
                    console.state.connection.clone(),
                    console.state.database.clone(),
                )
            });
        match here {
            Some((Some(connection), database)) => {
                self.start_db_console(
                    ConsoleTarget::NewTab,
                    connection,
                    database,
                    None,
                    window,
                    cx,
                );
            }
            _ => {
                let Some(worktree) = self.active.clone() else {
                    return;
                };
                let id = self.open_console(worktree, true, window, cx);
                self.reveal_console(id, window, cx);
                self.persist_session(cx);
            }
        }
    }

    // — Painting one ——————————————————————————————————————————————

    pub(super) fn render_db_console(
        &mut self,
        id: ConsoleId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(console) = self.console(id) else {
            return div().into_any_element();
        };
        let editor = console.input.clone();
        let split = console.split.clone();
        let surface = Surface::Query(id);
        let vim = crate::ui::settings::Settings::global(cx).vim_mode;
        // The same four pieces the file editor installs, on the same harness:
        // see `ui::surface`. SQL is code, read and written the same way, and the
        // console was the one code panel that had none of them.
        self.advance_surface_scroll(&surface, &editor, window, cx);
        self.sync_block_cursor(&surface, vim, cx);
        // The occurrences of the last search, lit as `Ctrl+F` lights them:
        // see `sync_search_matches`.
        self.sync_search_matches(&surface, vim, cx);
        // And the occurrence the bar has just jumped to, put in the middle of
        // the panel rather than on its edge: see `centre_search_match`.
        self.centre_search_match(&surface, cx);
        let bar = self.render_console_bar(id, cx);
        let results = self.render_db_results(id, window, cx);
        // SQL is code: same family, same size as the diff and the file editor,
        // and the line height said explicitly — `Input`'s rem-based default is
        // deaf to the text size (see the file editor, `explorer.rs`).
        let mono = cx.theme().mono_font_family.clone();
        let code_size = px(crate::ui::settings::Settings::global(cx).diff_font_size);
        let border = cx.theme().border;
        let rank = id.0 as usize;
        v_flex()
            // **Keyed by the console and not by the panel**: two consoles can be
            // side by side in a split, and one element id used twice is two
            // views gpui takes for one — the scroll of the second landing in the
            // first, which is the same trap `Surface::File` exists for.
            .id(("db-console", rank))
            // The console's context: it is what gives `Ctrl+Enter` to the query
            // rather than to the commit, and `Ctrl+C` to the grid rather than to
            // the diff.
            .key_context(crate::ui::shortcuts::query_context())
            .size_full()
            .child(bar)
            .child(
                // The editor and the grid share the height, and the share is
                // adjustable: one writes a twenty-line query, then reads three
                // hundred rows of result, and no fixed proportion suits both.
                v_resizable(("db-split", rank))
                    .with_state(&split)
                    .child(
                        resizable_panel()
                            .size(px(180.))
                            .size_range(px(72.)..px(640.))
                            .child(
                                div()
                                    .id(("db-query-editor", rank))
                                    .relative()
                                    .size_full()
                                    .overflow_hidden()
                                    .border_b_1()
                                    .border_color(border)
                                    // The four keys vim takes before the
                                    // editor sees them, installed only when the
                                    // mode is on: see `surface::vim_capture`.
                                    .map(|el| match vim {
                                        true => {
                                            let el = el.key_context(
                                                crate::ui::shortcuts::editor_vim_context(),
                                            );
                                            crate::ui::surface::vim_capture(
                                                el,
                                                Surface::Query(id),
                                                &cx.entity(),
                                            )
                                        }
                                        false => el,
                                    })
                                    .child(
                                        // No card of its own: see the file
                                        // editor in `explorer.rs`. The seam
                                        // with the grid below is the
                                        // container's own bottom border — the
                                        // resize handle paints nothing at
                                        // rest.
                                        Editor::new(&editor)
                                            .appearance(false)
                                            .font_family(mono)
                                            .text_size(code_size)
                                            .line_height(crate::ui::diff_view::line_height(
                                                code_size,
                                            ))
                                            .h_full(),
                                    )
                                    // The wheel, taken before the editor sees
                                    // it: see `surface::wheel_capture`.
                                    .child(crate::ui::surface::wheel_capture(
                                        Surface::Query(id),
                                        &cx.entity(),
                                    )),
                            ),
                    )
                    .child(resizable_panel().child(results)),
            )
            .into_any_element()
    }

    fn render_console_bar(&mut self, id: ConsoleId, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(console) = self.console(id) else {
            return div().into_any_element();
        };
        let state = &console.state;
        let target = match (&state.connection, &state.database) {
            (Some(connection), Some(database)) => format!("{} · {database}", connection.label()),
            (Some(connection), None) => connection.label(),
            (None, _) => String::new(),
        };
        let running = state.running;
        let failed = state.error.is_some();
        let exporting = state.exporting.is_some();
        let has_result = state.has_columns && state.shown > 0;
        // The mode, where the eye already is: on the console's own bar, as the
        // file editor puts it on the file's.
        let vim = crate::ui::settings::Settings::global(cx).vim_mode;
        let mode = console.host.vim.mode();
        let hint = console
            .host
            .vim
            .prompt()
            .unwrap_or_else(|| console.host.vim.pending().to_string());
        let status = self.db_status_text(id);
        let rank = id.0 as usize;
        h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new(("db-run", rank))
                    .ghost()
                    .small()
                    .icon(icon("play"))
                    .tooltip(tr!("db-run"))
                    .disabled(running)
                    .on_click(cx.listener(move |this, _, _window, cx| this.run_db_query(id, cx))),
            )
            .child(
                div()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(target)),
            )
            .child(div().flex_1())
            .when(vim, |el| el.child(self.render_vim_mode(mode, &hint, cx)))
            .child(
                div()
                    .truncate()
                    .text_xs()
                    .text_color(if failed {
                        cx.theme().danger
                    } else {
                        cx.theme().muted_foreground
                    })
                    .child(status),
            )
            .children(self.render_db_pagination(id, cx))
            .child(self.render_page_size(cx))
            .child(
                Button::new(("db-copy", rank))
                    .ghost()
                    .small()
                    .icon(icon("copy"))
                    .tooltip(tr!("db-copy-result"))
                    .disabled(!has_result)
                    .on_click(cx.listener(move |this, _, _window, cx| this.copy_db_result(id, cx))),
            )
            .child(
                Button::new(("db-export", rank))
                    .ghost()
                    .small()
                    .icon(icon("download"))
                    .tooltip(tr!("db-export"))
                    .disabled(!has_result || exporting)
                    .on_click(cx.listener(move |this, _, _window, cx| this.export_db_csv(id, cx))),
            )
            // No `×` of our own any more: the tab has one, which is the cross
            // every other document of the centre is closed by. A second one on
            // the bar underneath would be the same gesture twice, an inch apart.
            .into_any_element()
    }

    /// What the bar says about the displayed window.
    fn db_status_text(&self, id: ConsoleId) -> SharedString {
        let Some(console) = self.console(id) else {
            return SharedString::default();
        };
        let state = &console.state;
        if state.running {
            return tr!("db-running");
        }
        if state.error.is_some() {
            return tr!("db-failed");
        }
        let ms = state.elapsed_ms;
        if state.sent.is_none() {
            return SharedString::default();
        }
        if !state.has_columns {
            let affected = state.affected.unwrap_or(0);
            return tr!("db-affected", { n: affected, ms: ms });
        }
        if state.offset == 0 && !state.more {
            return tr!("db-row-count", { n: state.shown, ms: ms });
        }
        let first = state.offset + 1;
        let last = state.offset + state.shown;
        tr!("db-row-range", {
            first: first,
            last: last,
            more: if state.more { "+" } else { "" },
            ms: ms,
        })
    }

    /// The two gestures that move the window, when the result overflows it.
    fn render_db_pagination(
        &mut self,
        id: ConsoleId,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let state = &self.console(id)?.state;
        if !state.has_columns || (state.offset == 0 && !state.more) {
            return None;
        }
        let (offset, shown, more) = (state.offset, state.shown, state.more);
        let size = Settings::global(cx).db_page_size.max(1);
        let rank = id.0 as usize;
        Some(
            h_flex()
                .gap_0p5()
                .child(
                    Button::new(("db-first", rank))
                        .ghost()
                        .small()
                        .icon(icon("chevrons-left"))
                        .tooltip(tr!("db-first-page"))
                        .disabled(offset == 0)
                        .on_click(
                            cx.listener(move |this, _, _window, cx| this.page_db_query(id, 0, cx)),
                        ),
                )
                .child(
                    Button::new(("db-previous", rank))
                        .ghost()
                        .small()
                        .icon(icon("chevron-left"))
                        .tooltip(tr!("db-previous-page"))
                        .disabled(offset == 0)
                        .on_click(cx.listener(move |this, _, _window, cx| {
                            this.page_db_query(id, offset.saturating_sub(size), cx)
                        })),
                )
                .child(
                    Button::new(("db-next", rank))
                        .ghost()
                        .small()
                        .icon(icon("chevron-right"))
                        .tooltip(tr!("db-next-page"))
                        .disabled(!more)
                        .on_click(cx.listener(move |this, _, _window, cx| {
                            this.page_db_query(id, offset + shown, cx)
                        })),
                ),
        )
    }

    /// The window's size, which is a setting: it is chosen once for every
    /// console, not on every query.
    fn render_page_size(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = Settings::global(cx).db_page_size.max(1);
        Button::new("db-page-size")
            .ghost()
            .small()
            .label(SharedString::from(current.to_string()))
            .tooltip(tr!("db-page-size"))
            .dropdown_menu(move |menu, _window, _cx| {
                PAGE_SIZES.iter().fold(menu, |menu, size| {
                    let size = *size;
                    menu.item(
                        PopupMenuItem::new(SharedString::from(size.to_string()))
                            .checked(size == current)
                            .on_click(move |_, _window, cx| {
                                Settings::update_global(cx, |settings| {
                                    settings.db_page_size = size;
                                });
                            }),
                    )
                })
            })
    }

    fn render_db_results(
        &mut self,
        id: ConsoleId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let centered = |message: SharedString, error: bool, cx: &Context<Self>| {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .p_4()
                .text_sm()
                .text_color(if error {
                    cx.theme().danger
                } else {
                    cx.theme().muted_foreground
                })
                .child(message)
                .into_any_element()
        };
        let Some(console) = self.console(id) else {
            return div().into_any_element();
        };
        let state = &console.state;
        let rank = id.0 as usize;
        if let Some(error) = state.error.clone() {
            // The engine's error, as it is: it is what says the offending line
            // and column, and rewording it would only add approximations.
            return div()
                .id(("db-error", rank))
                .size_full()
                .overflow_scroll()
                .p_3()
                .text_xs()
                .font_family(cx.theme().mono_font_family.clone())
                .text_color(cx.theme().danger)
                .child(error)
                .into_any_element();
        }
        if state.sent.is_none() {
            return centered(tr!("db-run-hint"), false, cx);
        }
        if !state.has_columns {
            return centered(
                tr!("db-affected-short", { n: state.affected.unwrap_or(0) }),
                false,
                cx,
            );
        }
        if state.shown == 0 {
            return centered(tr!("db-no-rows"), false, cx);
        }
        // Wheel smoothing, as everywhere else: the grid routinely runs to a
        // thousand rows, and a notch jumping three lines at once makes the eye
        // lose its place. The table paints its own bars, hence `smoothed` and
        // not `scrolled`.
        //
        // **One key per console**, as the file editor keys by path: a shared
        // motion hands one grid's destination to the other.
        let table = console.table.clone();
        let handle = table.read(cx).vertical_scroll_handle.clone();
        self.smoothed(
            SharedString::from(format!("db-results:{rank}")),
            &handle,
            Axes::Vertical,
            window,
            DataTable::new(&table).stripe(true).bordered(false),
            cx,
        )
        .into_any_element()
    }
}

/// Completes what `db::complete` decides, and paints nothing of its own.
///
/// **The provider filters and ranks itself**: gpui-component's menu shows what
/// it is given, in the order it is given, without dropping anything. A
/// three-hundred-table schema would otherwise offer three hundred rows on the
/// first letter typed — and a list cut before it is ranked is how the column
/// one meant never shows up.
pub struct SqlCompletions {
    pub schema: Rc<RefCell<SchemaIndex>>,
}

impl CompletionProvider for SqlCompletions {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _trigger: CompletionContext,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<CompletionResponse>> {
        let source = text.to_string();
        let word = db::complete::word_range(&source, offset);
        let candidates = {
            let schema = self.schema.borrow();
            db::complete::candidates(
                &source,
                offset,
                &schema.tables,
                &schema.foreign_keys,
                db::complete::KEYWORDS,
            )
        };

        // The replacement is given explicitly: the editor's fallback range
        // starts at the first character of the **trigger** word, which takes in
        // the `users.` of a qualified column — the table would be replaced by
        // its column.
        let range = lsp_types::Range {
            start: text.offset_to_position(word.start),
            end: text.offset_to_position(word.end),
        };
        let completions: Vec<CompletionItem> = candidates
            .into_iter()
            .map(|candidate| CompletionItem {
                // No `filter_text`: the menu underlines its **length** from the
                // start of the label, so filling it in — which is what LSP asks
                // for — underlines every entry whole. Left out, the underline is
                // the typed word's length, which is what a prefix match claims.
                kind: Some(match candidate.kind {
                    db::complete::Kind::Table => CompletionItemKind::CLASS,
                    db::complete::Kind::Column => CompletionItemKind::FIELD,
                    db::complete::Kind::Keyword => CompletionItemKind::KEYWORD,
                    db::complete::Kind::Join => CompletionItemKind::SNIPPET,
                }),
                detail: candidate.detail,
                text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                    range,
                    new_text: candidate.text,
                })),
                label: candidate.label,
                ..Default::default()
            })
            .collect();
        let _ = cx;
        Task::ready(Ok(CompletionResponse::Array(completions)))
    }

    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _cx: &mut App) -> bool {
        new_text
            .chars()
            .last()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.')
    }
}

/// The editor/results split state, created once with the console that holds it.
pub fn split_state(cx: &mut App) -> gpui::Entity<ResizableState> {
    cx.new(|_| ResizableState::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A three-row result, with one null and one value carrying a comma.
    fn results() -> Results {
        Results {
            rows: db::Rows {
                columns: vec!["id".into(), "email".into(), "name".into()],
                rows: vec![
                    vec![Some("1".into()), Some("a@x".into()), Some("Ada".into())],
                    vec![Some("2".into()), Some("b@x".into()), None],
                    vec![Some("3".into()), Some("c,d@x".into()), Some("Eve".into())],
                ],
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// The rectangle reads both ways: one drags upwards and leftwards just as
    /// well, and the anchor stays the first click's.
    #[test]
    fn a_selection_reads_the_same_from_either_corner() {
        let mut results = results();
        results.press(2, 2, false);
        assert!(results.drag_to(0, 1));
        assert!(!results.drag_to(0, 1), "nothing moved, nothing to repaint");

        let selection = results.selection.unwrap();
        assert_eq!(selection.anchor, (2, 2), "l'ancre est le premier clic");
        assert!(selection.contains(1, 1) && selection.contains(0, 2));
        assert!(!selection.contains(0, 0), "la colonne 0 est hors du bloc");
        assert_eq!(selection.count(), 6);
    }

    /// Shift+click moves the cursor and keeps the anchor; a bare click starts over.
    #[test]
    fn a_shift_click_extends_and_a_plain_click_restarts() {
        let mut results = results();
        results.press(0, 0, false);
        results.press(2, 1, true);
        assert_eq!(results.selection.unwrap().anchor, (0, 0));
        assert_eq!(results.selection.unwrap().count(), 6);

        results.press(1, 1, false);
        assert_eq!(results.selection.unwrap().count(), 1);
    }

    /// A single cell comes out as it is: it is a value that will be pasted into
    /// a query, and quoting it would be a chore on every paste.
    #[test]
    fn a_single_cell_is_copied_raw() {
        let mut results = results();
        results.press(2, 1, false);
        assert_eq!(results.selected_text(false).unwrap(), "c,d@x");

        // With the headers it becomes a table again — and the comma is no longer
        // quoted, the clipboard separating with tabs.
        assert_eq!(results.selected_text(true).unwrap(), "email\nc,d@x\n");
    }

    /// A block comes out in columns, and a null value comes out empty rather
    /// than under the word "NULL", which only means something in the grid.
    #[test]
    fn a_block_is_copied_in_columns_and_null_is_empty() {
        let mut results = results();
        results.press(0, 1, false);
        results.drag_to(1, 2);
        assert_eq!(results.selected_text(false).unwrap(), "a@x\tAda\nb@x\t\n");

        results.select_all();
        assert_eq!(results.selection.unwrap().count(), 9);
        assert_eq!(results.all_text(), results.selected_text(true).unwrap());
    }

    /// A row is copied with its column names: it is what gets read back in a
    /// message, where "3, c,d@x, Eve" would say nothing.
    #[test]
    fn a_row_is_copied_under_its_headers() {
        let results = results();
        assert_eq!(
            results.row_text(2).unwrap(),
            "id\temail\tname\n3\tc,d@x\tEve\n"
        );
        assert!(results.row_text(9).is_none());
    }

    /// An empty result has nothing to select: `Ctrl+A` must not lay a rectangle
    /// over zero cells, which copying would read out of bounds.
    #[test]
    fn there_is_nothing_to_select_in_an_empty_result() {
        let mut results = Results::default();
        results.select_all();
        assert!(results.selection.is_none());
        assert!(results.selected_text(false).is_none());
    }
}
