//! The SQL console.
//!
//! An editor at the top, the result underneath: it is PhpStorm's console, and it
//! is the shape of every one already under our fingers.
//!
//! **It takes the diff's place**, like the built-in editor and for the same
//! reason: one looks at one *or* the other, and one more tab in the centre would
//! be a round trip on every query. It is also what makes it reachable —
//! gpui-component's dock cannot activate a tab from code
//! (`TabPanel::set_active_ix` is private), so a panel of its own would have
//! opened without showing itself.
//!
//! **One console at a time.** Zed opens one per tab; here the central slot is
//! unique, and two stacked consoles would need a tab bar of our own. Opening a
//! console on another table replaces the previous one, whose query is in the
//! editor's history anyway.
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
    div, prelude::*, px, App, Context, Focusable as _, SharedString, Task, WeakEntity, Window,
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
    /// An export has gone out and not come back.
    pub exporting: bool,
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
        self.report(cx, move |this, cx| this.sort_db_query(next, cx));
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
                table
                    .delegate()
                    .report(cx, move |this, cx| this.sort_db_query(next, cx));
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
                            window.defer(cx, move |window, cx| {
                                app.update(cx, |this, cx| {
                                    this.follow_db_key(row, column, window, cx);
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
                            app.update(cx, |this, cx| this.run_db_sql(sql.clone(), window, cx))
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
                    copy.update(cx, |this, cx| this.copy_db_selection(false, cx))
                        .ok();
                }),
        )
        .item(
            PopupMenuItem::new(tr!("db-copy-with-headers"))
                .icon(icon("table"))
                .disabled(!selected)
                .on_click(move |_, _window, cx| {
                    headers
                        .update(cx, |this, cx| this.copy_db_selection(true, cx))
                        .ok();
                }),
        )
        .separator()
        .item(
            PopupMenuItem::new(tr!("db-copy-row"))
                .icon(icon("copy"))
                .on_click(move |_, _window, cx| {
                    line.update(cx, |this, cx| this.copy_db_row(row, cx)).ok();
                }),
        )
        .item(
            PopupMenuItem::new(tr!("db-copy-result"))
                .icon(icon("copy"))
                .on_click(move |_, _window, cx| {
                    all.update(cx, |this, cx| this.copy_db_all(cx)).ok();
                }),
        )
        .separator()
        .item(
            PopupMenuItem::new(tr!("db-export"))
                .icon(icon("download"))
                .on_click(move |_, _window, cx| {
                    export.update(cx, |this, cx| this.export_db_csv(cx)).ok();
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
        self.report(cx, |this, cx| this.extend_db_rows(cx));
    }
}

impl ClaudhubApp {
    /// Opens the console on a connection, and possibly on a table.
    ///
    /// A table gives a `SELECT * FROM …` **and runs it**: "query this table"
    /// showing nothing until the button has been found would be a gesture half
    /// done.
    pub(super) fn start_db_console(
        &mut self,
        connection: db::Connection,
        database: Option<String>,
        table: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Read before anything moves: opening a table replaces the query one
        // was reading, and that is the thing which had nowhere to be written.
        let from = self.here(cx);
        let changed =
            self.query.connection.as_ref() != Some(&connection) || self.query.database != database;
        self.query.connection = Some(connection.clone());
        self.query.database = database.clone();
        if changed {
            self.query.error = None;
            self.query.sent = None;
            self.query.sort = None;
            self.query.can_sort = false;
            self.set_db_rows(db::Rows::default(), cx);
            self.index_db_schema(&connection, database.as_deref(), cx);
        }
        if let Some(table) = table {
            let quoted = match connection.engine {
                db::Engine::Sqlite => format!("\"{table}\""),
                db::Engine::Mysql => format!("`{table}`"),
            };
            // No `LIMIT`: the result window already stands for one, and a bound
            // written into the text would outlive the query one writes over it.
            let sql = format!("SELECT * FROM {quoted};");
            self.db_query_input.update(cx, |state, cx| {
                state.set_value(sql.clone(), window, cx);
            });
            self.run_db_query(cx);
            self.record_step(
                from,
                crate::ui::jumps::Place::Query {
                    connection: connection.key(),
                    database: database.clone(),
                    sql,
                },
                cx,
            );
        } else {
            self.record_step(
                from,
                crate::ui::jumps::Place::Screen(crate::ui::workspace::Workspace::Db),
                cx,
            );
        }
        // Opening a console calls up the databases screen: the gesture comes
        // from the schema tree, which lives there, but also from the menu of a
        // table opened elsewhere.
        //
        // `enter_workspace` and not `travel_to`: the step has just been
        // written, and it names the query rather than the room it is read in.
        self.enter_workspace(crate::ui::workspace::Workspace::Db, window, cx);
        self.set_panel_visible(crate::ui::panels::ConsolePanel::NAME, true, cx);
        self.persist_session(cx);
        cx.notify();
    }

    /// Puts the console back where the previous session left it.
    ///
    /// Everything `start_db_console` does apart from its two side effects: it
    /// neither calls up the databases screen nor unhides the panel. Restoring
    /// is not a gesture — the screen that comes back is the arriving worktree's
    /// own, put back a step earlier by `settle_place`, and a console hidden when
    /// quitting must stay hidden.
    ///
    /// The query is put in the editor and **not sent**: what one comes back to
    /// is the text one was writing, and replaying a `SELECT` nobody asked for
    /// is a query against a server one did not ask to reach.
    pub(super) fn reopen_db_console(
        &mut self,
        connection: db::Connection,
        database: Option<String>,
        query: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.query.connection = Some(connection.clone());
        self.query.database = database.clone();
        self.db_query_input.update(cx, |state, cx| {
            state.set_value(query, window, cx);
        });
        self.index_db_schema(&connection, database.as_deref(), cx);
        cx.notify();
    }

    /// Closes the console and gives the centre back to the diff.
    pub(super) fn close_db_console(&mut self, cx: &mut Context<Self>) {
        self.reset_db_console(cx);
        self.persist_session(cx);
        cx.notify();
    }

    /// Empties the console without filing anything.
    ///
    /// The half `close_db_console` shares with an arrival in another worktree:
    /// there is one console for the whole window, so its place has to be
    /// cleared before the next one is put in it — and writing the store in
    /// between would file an empty console under a checkout that has one.
    pub(super) fn reset_db_console(&mut self, cx: &mut Context<Self>) {
        self.query = QueryState::default();
        self.set_db_rows(db::Rows::default(), cx);
    }

    pub(super) fn db_console_open(&self) -> bool {
        self.query.connection.is_some()
    }

    /// Asks for the names the console will complete.
    ///
    /// It is the same command as the panel's: if the tree has already indexed
    /// this database, the answer fills both.
    fn index_db_schema(
        &mut self,
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
        self.db_schema.borrow_mut().database = None;
        self.git.send(Cmd::DbAllColumns {
            connection: connection.clone(),
            database,
        });
    }

    /// Files a schema that has just arrived, if it is the console's.
    pub(super) fn db_schema_indexed(
        &mut self,
        key: &str,
        database: &str,
        columns: &BTreeMap<String, Vec<db::Column>>,
        cx: &mut Context<Self>,
    ) {
        let Some(connection) = self.query.connection.as_ref() else {
            return;
        };
        if connection.key() != key {
            return;
        }
        let expected = match connection.engine {
            db::Engine::Sqlite => "main",
            db::Engine::Mysql => self.query.database.as_deref().unwrap_or_default(),
        };
        if expected != database {
            return;
        }
        let mut index = self.db_schema.borrow_mut();
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
        drop(index);
        // A result shown before the index arrived carries no key yet. The links
        // are recomputed rather than the table refreshed: `refresh` would put
        // the scrolling back to the top, and what changes here is only what the
        // cells are painted with.
        let columns = self.db_table.read(cx).delegate().rows.columns.clone();
        let links = self.db_links(&columns);
        self.db_table.update(cx, |state, cx| {
            state.delegate_mut().links = links;
            cx.notify();
        });
    }

    /// Tells the result grid whether the system key is held.
    ///
    /// Pushed and not read: see `Results::armed`. It costs a frame at each flip
    /// of the modifier, and only when a console is on screen.
    pub(super) fn arm_db_follow(&mut self, armed: bool, cx: &mut Context<Self>) {
        self.db_table.update(cx, |state, cx| {
            if state.delegate().armed != armed {
                state.delegate_mut().armed = armed;
                cx.notify();
            }
        });
    }

    /// Follows the foreign key a cell carries.
    ///
    /// The gesture is the system-key click and the menu entry, and both end in
    /// `run_db_sql` — two ways of making one gesture that did not land in the
    /// same place would be one too many.
    pub(super) fn follow_db_key(
        &mut self,
        row: usize,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let results = self.db_table.read(cx).delegate();
        let sql = results
            .link(row, column)
            .map(|(target, value)| db::link::select_row(results.engine, target, value));
        if let Some(sql) = sql {
            self.run_db_sql(sql, window, cx);
        }
    }

    /// Puts a query into the editor and runs it.
    ///
    /// It is what opening a table from the tree does, and following a key does
    /// the same: the text is what one goes on to adjust, and the previous query
    /// is one row up in the history panel — which is what makes this
    /// overwriting bearable.
    pub(super) fn run_db_sql(&mut self, sql: String, window: &mut Window, cx: &mut Context<Self>) {
        let from = self.here(cx);
        self.db_query_input.update(cx, |state, cx| {
            state.set_value(sql.clone(), window, cx);
        });
        self.run_db_query(cx);
        if let Some(connection) = self.query.connection.as_ref() {
            let to = crate::ui::jumps::Place::Query {
                connection: connection.key(),
                database: self.query.database.clone(),
                sql,
            };
            self.record_step(from, to, cx);
        }
    }

    /// Puts a query of the trail back, and runs it.
    ///
    /// It runs, where restoring a session does not: a step back is a gesture,
    /// asked for now, and what one is coming back to is the **result** — the
    /// row a foreign key was followed from. Putting the text back and leaving
    /// the previous result on screen would be a back button that undoes the
    /// half one cannot see.
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
        let elsewhere = self.query.connection.as_ref().map(|c| c.key()) != Some(connection)
            || self.query.database != database;
        if elsewhere {
            self.reopen_db_console(target, database, String::new(), window, cx);
        }
        self.enter_workspace(crate::ui::workspace::Workspace::Db, window, cx);
        self.set_panel_visible(crate::ui::panels::ConsolePanel::NAME, true, cx);
        self.db_query_input.update(cx, |state, cx| {
            state.set_value(sql, window, cx);
        });
        self.run_db_query(cx);
        self.query.record = false;
        self.persist_session(cx);
        cx.notify();
    }

    /// Runs whatever is in the editor.
    ///
    /// The sort starts again from scratch: it is about a column of the result,
    /// and nothing says the new query has the same one.
    pub(super) fn run_db_query(&mut self, cx: &mut Context<Self>) {
        let sql = self.db_query_input.read(cx).value().to_string();
        if sql.trim().is_empty() {
            return;
        }
        self.query.sent = Some(sql);
        self.query.sort = None;
        self.query.can_sort = false;
        self.query.record = true;
        self.send_db_query(0, false, cx);
    }

    /// Sorts the result, or removes its sort.
    ///
    /// The window goes back to its start: the rows that filled it are no longer
    /// the first of anything.
    pub(super) fn sort_db_query(&mut self, sort: Option<Sort>, cx: &mut Context<Self>) {
        if !self.query.can_sort || self.query.sort == sort {
            return;
        }
        self.query.sort = sort;
        self.query.record = false;
        // The arrow follows the gesture and not the answer: a query sometimes
        // takes a second, and a header that does not move reads as a lost click.
        self.db_table.update(cx, |state, cx| {
            state.delegate_mut().sort = sort;
            state.refresh(cx);
        });
        self.send_db_query(0, false, cx);
    }

    /// Moves the window.
    pub(super) fn page_db_query(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.query.record = false;
        self.send_db_query(offset, false, cx);
    }

    /// Extends the window: scrolling has reached the bottom.
    pub(super) fn extend_db_rows(&mut self, cx: &mut Context<Self>) {
        if self.query.running || !self.query.more {
            // The table put itself in a waiting state before calling us; without
            // this, it would never come out of it.
            self.db_table.update(cx, |state, _| {
                state.delegate_mut().loading = false;
            });
            return;
        }
        self.query.record = false;
        let next = self.query.offset + self.query.shown;
        self.send_db_query(next, true, cx);
    }

    /// The query as it really goes out: the one that was run, and the sort asked
    /// for around it.
    fn effective_sql(&self) -> Option<String> {
        let sent = self.query.sent.clone()?;
        match self.query.sort {
            Some(sort) => Some(db::order_by(&sent, sort.column, sort.ascending).unwrap_or(sent)),
            None => Some(sent),
        }
    }

    fn send_db_query(&mut self, offset: usize, append: bool, cx: &mut Context<Self>) {
        let Some(connection) = self.query.connection.clone() else {
            return;
        };
        let Some(sql) = self.effective_sql() else {
            return;
        };
        let limit = Settings::global(cx).db_page_size.max(1);
        self.query.request += 1;
        self.query.appending = append;
        self.query.running = true;
        self.query.error = None;
        self.git.send(Cmd::DbQuery {
            connection,
            database: self.query.database.clone(),
            sql,
            offset,
            limit,
            request: self.query.request,
        });
        cx.notify();
    }

    /// A query's result.
    ///
    /// It is **dropped if it does not answer the last request**: one restarts
    /// before the previous has come back — by changing page, by sorting, by
    /// scrolling down — and showing the late answer would replace what is being
    /// looked at with what is not.
    pub(super) fn db_rows_arrived(
        &mut self,
        request: u64,
        rows: crate::runtime::protocol::DbResult<db::Rows>,
        elapsed_ms: u64,
        cx: &mut Context<Self>,
    ) {
        if self.query.request != request {
            return;
        }
        self.query.running = false;
        self.query.elapsed_ms = elapsed_ms;
        // Filed in the history, and only what a gesture asked for — see
        // `QueryState::record`.
        if std::mem::take(&mut self.query.record) {
            match &rows {
                Ok(page) => self.record_sql_query(
                    true,
                    (!page.columns.is_empty()).then_some(page.rows.len()),
                    page.affected,
                    None,
                    elapsed_ms,
                    cx,
                ),
                Err(message) => {
                    self.record_sql_query(false, None, None, Some(message.clone()), elapsed_ms, cx)
                }
            }
        }
        match rows {
            Ok(rows) => {
                self.query.error = None;
                let sent = self.query.sent.clone().unwrap_or_default();
                self.query.can_sort = db::can_order(&sent, &rows.columns);
                self.query.affected = rows.affected;
                if self.query.appending {
                    self.query.more = rows.more;
                    self.query.shown += rows.rows.len();
                    self.extend_db_table(rows, cx);
                } else {
                    self.query.offset = rows.offset;
                    self.query.shown = rows.rows.len();
                    self.query.more = rows.more;
                    self.query.has_columns = !rows.columns.is_empty();
                    self.set_db_rows(rows, cx);
                }
            }
            Err(message) => {
                self.query.error = Some(message.into());
                self.query.has_columns = false;
                self.query.can_sort = false;
                self.query.more = false;
                self.set_db_rows(db::Rows::default(), cx);
            }
        }
        cx.notify();
    }

    /// Replaces the table's content.
    ///
    /// The table is an entity created once: rebuilding it on every result would
    /// lose the widths just adjusted with the mouse and would put the scrolling
    /// back to the top in the middle of paging.
    fn set_db_rows(&mut self, rows: db::Rows, cx: &mut Context<Self>) {
        let links = self.db_links(&rows.columns);
        let mut results = Results::new(rows, links, &self.query, cx);
        // A result that lands while the key is held is followable straight
        // away: the flag is only pushed when the modifier *flips*, and paging
        // with `Ctrl` down would otherwise paint a grid that says nothing can
        // be followed until one lets go of it.
        results.armed = self.follow_armed;
        self.db_table.update(cx, |state, cx| {
            *state.delegate_mut() = results;
            state.refresh(cx);
        });
    }

    /// Which of a result's columns can be followed.
    ///
    /// Empty as long as the schema has not been indexed — the answer comes
    /// several seconds after the console opens, and `db_schema_indexed` asks
    /// again then.
    fn db_links(&self, columns: &[String]) -> Vec<Option<db::link::Target>> {
        let Some(sql) = self.query.sent.as_deref() else {
            return vec![None; columns.len()];
        };
        let index = self.db_schema.borrow();
        db::link::targets(sql, columns, &index.foreign_keys)
    }

    /// Appends a page under the ones being looked at.
    ///
    /// The widths are **not** recomputed: they were derived from the first page,
    /// and revisiting them on every extension would move the columns under the
    /// eyes of whoever is scrolling. `refresh` is not called either — it would
    /// put the scrolling back to the top, which is exactly the opposite of what
    /// was just asked for.
    fn extend_db_table(&mut self, rows: db::Rows, cx: &mut Context<Self>) {
        self.db_table.update(cx, |state, cx| {
            let delegate = state.delegate_mut();
            delegate.rows.extend(rows);
            delegate.more = delegate.rows.more;
            delegate.loading = false;
            cx.notify();
        });
    }

    /// Selects the whole loaded result.
    pub(super) fn select_whole_db_result(&mut self, cx: &mut Context<Self>) {
        self.db_table.update(cx, |state, cx| {
            state.delegate_mut().select_all();
            cx.notify();
        });
    }

    /// Copies what is selected.
    ///
    /// A null cell copies **nothing** and not the word "NULL": that word is how
    /// the grid shows the absence of a value, and it means nothing once pasted
    /// elsewhere.
    pub(super) fn copy_db_selection(&mut self, headers: bool, cx: &mut Context<Self>) {
        let Some(text) = self.db_table.read(cx).delegate().selected_text(headers) else {
            return;
        };
        self.put_on_clipboard(text, cx);
    }

    /// Copies a whole row, with the column names above it.
    pub(super) fn copy_db_row(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(text) = self.db_table.read(cx).delegate().row_text(row) else {
            return;
        };
        self.put_on_clipboard(text, cx);
    }

    /// Copies the whole **loaded** result — not the query's whole result, which
    /// is what the export writes.
    pub(super) fn copy_db_all(&mut self, cx: &mut Context<Self>) {
        let table = self.db_table.read(cx);
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
    pub(super) fn copy_db_result(&mut self, cx: &mut Context<Self>) {
        if self.db_table.read(cx).delegate().selection.is_some() {
            self.copy_db_selection(false, cx);
        } else {
            self.copy_db_all(cx);
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
    pub(super) fn export_db_csv(&mut self, cx: &mut Context<Self>) {
        if self.query.exporting || self.query.sent.is_none() {
            return;
        }
        let directory = directories::UserDirs::new()
            .map(|dirs| dirs.home_dir().to_path_buf())
            .unwrap_or_else(std::env::temp_dir);
        let name = self
            .query
            .connection
            .as_ref()
            .map(|connection| format!("{}.csv", connection.label()))
            .unwrap_or_else(|| "export.csv".to_string());
        let path = cx.prompt_for_new_path(&directory, Some(&name));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = path.await else {
                return; // cancelled
            };
            let _ = this.update(cx, |this, cx| this.send_db_export(path, cx));
        })
        .detach();
    }

    fn send_db_export(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        let (Some(connection), Some(sql)) = (self.query.connection.clone(), self.effective_sql())
        else {
            return;
        };
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
        self.query.exporting = true;
        self.git.send(Cmd::DbExport {
            connection,
            database: self.query.database.clone(),
            sql,
            path,
        });
        cx.notify();
    }

    /// An export has come back. The path is given in full: it is the only thing
    /// one needs to remember to find it again.
    pub(super) fn db_exported(
        &mut self,
        path: std::path::PathBuf,
        rows: crate::runtime::protocol::DbResult<u64>,
        cx: &mut Context<Self>,
    ) {
        self.query.exporting = false;
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
                self.toast = Some(crate::ui::app::Toast {
                    text: SharedString::from(message),
                    error: true,
                });
            }
        }
        cx.notify();
    }

    pub(super) fn render_db_console(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let editor = self.db_query_input.clone();
        let split = self.db_split.clone();
        let vim = crate::ui::settings::Settings::global(cx).vim_mode;
        // The same four pieces the file editor installs, on the same harness:
        // see `ui::surface`. SQL is code, read and written the same way, and the
        // console was the one code panel that had none of them.
        self.advance_surface_scroll(&Surface::Query, &editor, window, cx);
        self.sync_block_cursor(&Surface::Query, vim, cx);
        // The occurrences of the last search, lit as `Ctrl+F` lights them:
        // see `sync_search_matches`.
        self.sync_search_matches(&Surface::Query, vim, cx);
        // And the occurrence the bar has just jumped to, put in the middle of
        // the panel rather than on its edge: see `centre_search_match`.
        self.centre_search_match(&Surface::Query, cx);
        let bar = self.render_console_bar(cx);
        let results = self.render_db_results(window, cx);
        // SQL is code: same family, same size as the diff and the file editor,
        // and the line height said explicitly — `Input`'s rem-based default is
        // deaf to the text size (see the file editor, `explorer.rs`).
        let mono = cx.theme().mono_font_family.clone();
        let code_size = px(crate::ui::settings::Settings::global(cx).diff_font_size);
        let border = cx.theme().border;
        v_flex()
            .id("db-console")
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
                v_resizable("db-split")
                    .with_state(&split)
                    .child(
                        resizable_panel()
                            .size(px(180.))
                            .size_range(px(72.)..px(640.))
                            .child(
                                div()
                                    .id("db-query-editor")
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
                                                crate::ui::shortcuts::query_editor_context(),
                                            );
                                            crate::ui::surface::vim_capture(el, Surface::Query, cx)
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
                                    .child(crate::ui::surface::wheel_capture(Surface::Query, cx)),
                            ),
                    )
                    .child(resizable_panel().child(results)),
            )
    }

    fn render_console_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let target = match (&self.query.connection, &self.query.database) {
            (Some(connection), Some(database)) => format!("{} · {database}", connection.label()),
            (Some(connection), None) => connection.label(),
            (None, _) => String::new(),
        };
        let running = self.query.running;
        let has_result = self.query.has_columns && self.query.shown > 0;
        // The mode, where the eye already is: on the console's own bar, as the
        // file editor puts it on the file's.
        let vim = crate::ui::settings::Settings::global(cx).vim_mode;
        let mode = self.db_host.vim.mode();
        let hint = self
            .db_host
            .vim
            .prompt()
            .unwrap_or_else(|| self.db_host.vim.pending().to_string());
        h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("db-run")
                    .ghost()
                    .xsmall()
                    .icon(icon("play"))
                    .tooltip(tr!("db-run"))
                    .disabled(running)
                    .on_click(cx.listener(|this, _, _window, cx| this.run_db_query(cx))),
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
                    .text_color(if self.query.error.is_some() {
                        cx.theme().danger
                    } else {
                        cx.theme().muted_foreground
                    })
                    .child(self.db_status_text()),
            )
            .children(self.render_db_pagination(cx))
            .child(self.render_page_size(cx))
            .child(
                Button::new("db-copy")
                    .ghost()
                    .xsmall()
                    .icon(icon("copy"))
                    .tooltip(tr!("db-copy-result"))
                    .disabled(!has_result)
                    .on_click(cx.listener(|this, _, _window, cx| this.copy_db_result(cx))),
            )
            .child(
                Button::new("db-export")
                    .ghost()
                    .xsmall()
                    .icon(icon("download"))
                    .tooltip(tr!("db-export"))
                    .disabled(!has_result || self.query.exporting)
                    .on_click(cx.listener(|this, _, _window, cx| this.export_db_csv(cx))),
            )
            .child(
                Button::new("db-close")
                    .ghost()
                    .xsmall()
                    .icon(icon("x"))
                    .tooltip(tr!("db-close-console"))
                    .on_click(cx.listener(|this, _, _window, cx| this.close_db_console(cx))),
            )
    }

    /// What the bar says about the displayed window.
    fn db_status_text(&self) -> SharedString {
        if self.query.running {
            return tr!("db-running");
        }
        if self.query.error.is_some() {
            return tr!("db-failed");
        }
        let ms = self.query.elapsed_ms;
        if self.query.sent.is_none() {
            return SharedString::default();
        }
        if !self.query.has_columns {
            let affected = self.query.affected.unwrap_or(0);
            return tr!("db-affected", { n: affected, ms: ms });
        }
        if self.query.offset == 0 && !self.query.more {
            return tr!("db-row-count", { n: self.query.shown, ms: ms });
        }
        let first = self.query.offset + 1;
        let last = self.query.offset + self.query.shown;
        tr!("db-row-range", {
            first: first,
            last: last,
            more: if self.query.more { "+" } else { "" },
            ms: ms,
        })
    }

    /// The two gestures that move the window, when the result overflows it.
    fn render_db_pagination(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.query.has_columns || (self.query.offset == 0 && !self.query.more) {
            return None;
        }
        let size = Settings::global(cx).db_page_size.max(1);
        let (offset, shown, more) = (self.query.offset, self.query.shown, self.query.more);
        Some(
            h_flex()
                .gap_0p5()
                .child(
                    Button::new("db-first")
                        .ghost()
                        .xsmall()
                        .icon(icon("chevrons-left"))
                        .tooltip(tr!("db-first-page"))
                        .disabled(offset == 0)
                        .on_click(
                            cx.listener(move |this, _, _window, cx| this.page_db_query(0, cx)),
                        ),
                )
                .child(
                    Button::new("db-previous")
                        .ghost()
                        .xsmall()
                        .icon(icon("chevron-left"))
                        .tooltip(tr!("db-previous-page"))
                        .disabled(offset == 0)
                        .on_click(cx.listener(move |this, _, _window, cx| {
                            this.page_db_query(offset.saturating_sub(size), cx)
                        })),
                )
                .child(
                    Button::new("db-next")
                        .ghost()
                        .xsmall()
                        .icon(icon("chevron-right"))
                        .tooltip(tr!("db-next-page"))
                        .disabled(!more)
                        .on_click(cx.listener(move |this, _, _window, cx| {
                            this.page_db_query(offset + shown, cx)
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
            .xsmall()
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
        if let Some(error) = self.query.error.clone() {
            // The engine's error, as it is: it is what says the offending line
            // and column, and rewording it would only add approximations.
            return div()
                .id("db-error")
                .size_full()
                .overflow_scroll()
                .p_3()
                .text_xs()
                .font_family(cx.theme().mono_font_family.clone())
                .text_color(cx.theme().danger)
                .child(error)
                .into_any_element();
        }
        if self.query.sent.is_none() {
            return centered(tr!("db-run-hint"), false, cx);
        }
        if !self.query.has_columns {
            return centered(
                tr!("db-affected-short", { n: self.query.affected.unwrap_or(0) }),
                false,
                cx,
            );
        }
        if self.query.shown == 0 {
            return centered(tr!("db-no-rows"), false, cx);
        }
        // Wheel smoothing, as everywhere else: the grid routinely runs to a
        // thousand rows, and a notch jumping three lines at once makes the eye
        // lose its place. The table paints its own bars, hence `smoothed` and
        // not `scrolled`.
        let handle = self.db_table.read(cx).vertical_scroll_handle.clone();
        self.smoothed(
            "db-results",
            &handle,
            Axes::Vertical,
            window,
            DataTable::new(&self.db_table).stripe(true).bordered(false),
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

/// The editor/results split state, created once with the window.
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
