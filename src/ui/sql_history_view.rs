//! The "History" panel: every query run, and what to do with one again.
//!
//! It sits beside the schema tree, in the databases screen's left column, and
//! that place is the point: one writes a query **while** looking at it, the way
//! one picks a table from the tree next door. Behind a popover it would be one
//! click away, which is how a history stops being consulted.
//!
//! The decisions — what is listed, what a repeat is, what a day is — live in
//! `sql_history.rs`, which knows nothing of gpui and is tested. Here there is
//! only plumbing.

use std::rc::Rc;

use gpui::{div, prelude::*, px, App, Context, Entity, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    menu::{ContextMenuExt, DropdownMenu as _, PopupMenu, PopupMenuItem},
    v_flex, v_virtual_list, ActiveTheme, Disableable, Sizable,
};

use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::find::Pane;
use crate::ui::icons::icon;
use crate::ui::sql_history::{Day, Entry, Filter, Reach, Row};

/// The bar's id, hence the key of the list's smoothing — one value for both.
const SCROLL: &str = "sql-history-bar";

impl ClaudhubApp {
    /// The console's connection, as the filter names it.
    ///
    /// Returned rather than filed in the filter: a key is built on the fly, and
    /// the filter borrows — the caller holds the string for as long as it uses
    /// it.
    fn sql_history_key(&self) -> Option<String> {
        self.focused_console()
            .and_then(|id| self.console(id))
            .and_then(|console| console.state.connection.as_ref())
            .map(|connection| connection.key())
    }

    /// Files a query that has just come back.
    ///
    /// Called from `db_rows_arrived`, which is where both halves are known: the
    /// query as it went out, and what the engine answered. A query that fails is
    /// recorded too — a typo one corrects is exactly what one wants back.
    /// The answer is given whole rather than taken apart at the call site: it
    /// is one thing the worker sent back, and four parameters of it were four
    /// chances to file a success under an error's shape.
    pub(super) fn record_sql_query(
        &mut self,
        console: crate::ui::db_query::ConsoleId,
        answer: &crate::runtime::protocol::DbResult<crate::db::Rows>,
        elapsed_ms: u64,
        cx: &mut Context<Self>,
    ) {
        let (ok, rows, affected, error) = match answer {
            Ok(page) => (
                true,
                (!page.columns.is_empty()).then_some(page.rows.len()),
                page.affected,
                None,
            ),
            Err(message) => (false, None, None, Some(message.clone())),
        };
        let Some(console) = self.console(console) else {
            return;
        };
        let (Some(connection), Some(sql)) =
            (console.state.connection.clone(), console.state.sent.clone())
        else {
            return;
        };
        let database = console.state.database.clone();
        if sql.trim().is_empty() {
            return;
        }
        self.sql_history.record(crate::ui::sql_history::Entry {
            at: crate::ui::sql_history::now(),
            worktree: self.active.clone().unwrap_or_default(),
            connection: connection.key(),
            label: connection.label(),
            database,
            sql,
            ok,
            rows,
            affected,
            // The first line only: a row is one line tall, and a stack trace in
            // a tooltip is not read either.
            error: error.map(|message| {
                message
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .unwrap_or_default()
                    .to_string()
            }),
            elapsed_ms,
            runs: 1,
        });
        self.persist_sql_history(cx);
    }

    /// Writes the journal.
    ///
    /// Serialised here — a millisecond — and written on a background thread: a
    /// journal is written on a gesture, never on a frame, and a vault on a
    /// synchronised disk is not something to wait for in the interface thread.
    pub(super) fn persist_sql_history(&mut self, cx: &mut Context<Self>) {
        let Some(json) = self.sql_history.serialise() else {
            return;
        };
        cx.background_executor()
            .spawn(async move { crate::ui::sql_history::History::write(&json) })
            .detach();
    }

    /// Puts a past query back in the console.
    ///
    /// The connection is looked up **in the settings by its key**, never taken
    /// from the entry: an entry names a connection, it does not describe one —
    /// it carries no password, and the address may have been corrected since.
    /// A connection since removed leaves the row in place, saying so, rather
    /// than opening a console that would fail on its first query.
    fn replay_sql_query(
        &mut self,
        entry: &Entry,
        run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(config) = crate::ui::settings::Settings::global(cx)
            .databases
            .iter()
            .find(|connection| connection.key() == entry.connection)
            .cloned()
        else {
            self.announce_error(tr!("sql-history-no-connection"), cx);
            cx.notify();
            return;
        };
        let sql = entry.sql.clone();
        // Into the console one is in, as everything the history recalls goes:
        // a row clicked is a query to adjust, not a reason for one more tab.
        self.start_db_console(
            crate::ui::db_query::ConsoleTarget::Current,
            config,
            entry.database.clone(),
            None,
            window,
            cx,
        );
        let Some(id) = self.focused_console() else {
            return;
        };
        if let Some(console) = self.console(id) {
            let input = console.input.clone();
            input.update(cx, |state, cx| {
                state.set_value(sql, window, cx);
            });
            if run {
                self.run_db_query(id, cx);
            } else {
                // The editor gets the focus: what one does with a recalled
                // query is adjust it before running it.
                let handle = gpui::Focusable::focus_handle(&input, cx);
                handle.focus(window, cx);
            }
        }
        cx.notify();
    }

    fn forget_sql_query(&mut self, entry: &Entry, cx: &mut Context<Self>) {
        self.sql_history.remove(entry.at, &entry.sql);
        self.persist_sql_history(cx);
        cx.notify();
    }

    /// Forgets what is being looked at, and only that.
    fn clear_sql_history(&mut self, cx: &mut Context<Self>) {
        let query = self.query(Pane::SqlHistory, cx);
        let key = self.sql_history_key();
        let filter = Filter {
            reach: self.sql_history_reach,
            worktree: self.active.as_deref(),
            connection: key.as_deref(),
            query: &query,
        };
        self.sql_history.clear(&filter);
        self.persist_sql_history(cx);
        cx.notify();
    }

    /// The queries already run, as the bottom half of the databases panel.
    ///
    /// **Not a panel of its own any more.** It was a tool window against the
    /// right edge, one tab away from the tree its rows are about — and what one
    /// does with a past query is run it against a schema one is looking at. The
    /// two halves share the panel's height, and the share is adjustable: one
    /// unfolds a schema, then reads a hundred rows of history, and no fixed
    /// proportion suits both.
    pub(super) fn render_sql_history(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let query = self.query(Pane::SqlHistory, cx);
        let find = self.render_find(Pane::SqlHistory, cx);
        let look = Look::of(cx);
        self.refresh_sql_history_list(&query, look);
        let Some(list) = self.sql_history_list.as_ref() else {
            return div().into_any_element();
        };
        let (shown, rows, sizes, count) = (
            list.shown.clone(),
            list.rows.clone(),
            list.sizes.clone(),
            list.count,
        );
        let bar = self.render_sql_history_bar(count, cx);
        if shown.is_empty() {
            return v_flex()
                .size_full()
                .child(bar)
                .children(find)
                .child(empty(&query, cx))
                .into_any_element();
        }

        let entity = cx.entity();
        let handle = self.sql_history_scroll.clone();
        // Read **here** and not in the row closure: that closure runs while the
        // application is being rendered, that is, in the middle of its own
        // update — reading the root entity there is the panic gpui refuses
        // (`cannot read … while it is already being updated`).
        let sent = self
            .focused_console()
            .and_then(|id| self.console(id))
            .and_then(|console| console.state.sent.clone());
        let build = move |index: usize, cx: &mut App| match rows.get(index) {
            Some(Row::Day(day)) => render_day(day, &look),
            Some(Row::Entry(at)) => match shown.get(*at) {
                Some(entry) => {
                    let current = sent.as_deref() == Some(entry.entry.sql.as_str());
                    render_entry(index, &shown, *at, current, &look, &entity, cx)
                }
                None => div().into_any_element(),
            },
            None => div().into_any_element(),
        };

        v_flex()
            .size_full()
            .child(bar)
            .children(find)
            .child(
                div().flex_1().min_h_0().child(
                    self.scrolled(
                        SCROLL,
                        &handle,
                        crate::ui::motion::Axes::Vertical,
                        window,
                        v_virtual_list(
                            cx.entity(),
                            "sql-history-rows",
                            sizes,
                            move |_, range, _window, cx| {
                                range.map(|index| build(index, cx)).collect::<Vec<_>>()
                            },
                        )
                        .size_full()
                        .track_scroll(&handle),
                        cx,
                    ),
                ),
            )
            .into_any_element()
    }

    /// Prepares what the list paints, and keeps it until something moves.
    ///
    /// All of it used to be done afresh on every frame: the filter walked the
    /// whole journal, its entries were cloned one by one, their days laid out
    /// and their heights measured — for a list that only changes on a gesture.
    /// The key is what the result depends on, `History::version` included: the
    /// journal is the one input that changes without the panel asking.
    fn refresh_sql_history_list(&mut self, query: &str, look: Look) {
        let key = Key {
            reach: self.sql_history_reach,
            worktree: self.active.clone(),
            connection: self.sql_history_key(),
            query: query.to_string(),
            version: self.sql_history.version,
            // The theme's row height, which is what the sizes are made of: a
            // list kept across a change of font size would reserve the old one.
            row: look.row,
            // A history read past midnight has to move its headings.
            today: chrono::Local::now().date_naive(),
        };
        if self
            .sql_history_list
            .as_ref()
            .is_some_and(|list| list.key == key)
        {
            return;
        }
        let filter = Filter {
            reach: key.reach,
            worktree: key.worktree.as_deref(),
            connection: key.connection.as_deref(),
            query,
        };
        let matching = self.sql_history.matching(&filter);
        let rows = Rc::new(crate::ui::sql_history::rows(&matching, key.today));
        let shown: Rc<Vec<Shown>> = Rc::new(matching.into_iter().map(Shown::of).collect());
        let sizes = Rc::new(
            rows.iter()
                .map(|row| match row {
                    Row::Day(_) => gpui::size(px(0.), look.day),
                    Row::Entry(_) => gpui::size(px(0.), look.row),
                })
                .collect::<Vec<_>>(),
        );
        // What the bar counts is not what the list shows: neither the search nor
        // the connection enters into it.
        let count = match key.reach {
            Reach::Worktree => self.sql_history.count_for(key.worktree.as_deref()),
            _ => self.sql_history.count_for(None),
        };
        self.sql_history_list = Some(HistoryList {
            key,
            shown,
            rows,
            sizes,
            count,
        });
    }

    fn render_sql_history_bar(&mut self, count: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let reach = self.sql_history_reach;
        let entity = cx.entity();
        h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(icon("clock").xsmall())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr!("sql-history-count", { n: count })),
            )
            .child(
                Button::new("sql-history-reach")
                    .ghost()
                    .xsmall()
                    .label(reach_label(reach))
                    .tooltip(tr!("sql-history-reach"))
                    .dropdown_menu(move |menu, _window, _cx| {
                        Reach::ALL.iter().fold(menu, |menu, option| {
                            let option = *option;
                            let entity = entity.clone();
                            menu.item(
                                PopupMenuItem::new(reach_label(option))
                                    .checked(option == reach)
                                    // A menu entry's click runs outside any
                                    // borrow of the application: this update is
                                    // legal here and would not be in the
                                    // closure that builds the menu.
                                    .on_click(move |_, _window, cx| {
                                        entity.update(cx, |this, cx| {
                                            this.sql_history_reach = option;
                                            cx.notify();
                                        });
                                    }),
                            )
                        })
                    }),
            )
            // **Its own magnifier.** `Ctrl+F` asks the panel the last click was
            // in, and this half shares its panel with the tree: the key goes to
            // the tree, so the filter of the rows below needs a way of its own.
            .child(
                Button::new("sql-history-find")
                    .ghost()
                    .xsmall()
                    .icon(icon("search"))
                    .tooltip(tr!("sql-history-find"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        let input = this.open_find_in(Pane::SqlHistory, window, cx);
                        gpui::Focusable::focus_handle(&input, cx).focus(window, cx);
                    })),
            )
            .child(
                Button::new("sql-history-clear")
                    .ghost()
                    .xsmall()
                    .icon(icon("eraser"))
                    .tooltip(tr!("sql-history-clear"))
                    .disabled(count == 0)
                    .on_click(cx.listener(|this, _, _window, cx| this.clear_sql_history(cx))),
            )
    }
}

/// A reach's label.
///
/// A match on literals and not `tr!(reach.key())`: `tr!` compiles its key, and
/// the catalogue test that checks every key exists reads literals too — a key
/// built at runtime would slip past both.
fn reach_label(reach: Reach) -> SharedString {
    match reach {
        Reach::Worktree => tr!("sql-history-reach-worktree"),
        Reach::Connection => tr!("sql-history-reach-connection"),
        Reach::All => tr!("sql-history-reach-all"),
    }
}

/// The list the panel shows, prepared once and kept until something moves.
///
/// The three vectors are handed to the virtual list, which reads them for every
/// visible row of every frame; rebuilding them there is what this exists to
/// stop. See `ClaudhubApp::refresh_sql_history_list` for what invalidates it.
pub(super) struct HistoryList {
    key: Key,
    shown: Rc<Vec<Shown>>,
    rows: Rc<Vec<Row>>,
    sizes: Rc<Vec<gpui::Size<gpui::Pixels>>>,
    /// What the bar counts, which is not the length of the list: the search is
    /// not part of it.
    count: usize,
}

/// Everything the prepared list depends on. It differs, it is rebuilt.
#[derive(PartialEq)]
struct Key {
    reach: Reach,
    worktree: Option<std::path::PathBuf>,
    connection: Option<String>,
    query: String,
    /// `History::version`: the journal is the one input that changes without
    /// the panel asking for it.
    version: u64,
    row: gpui::Pixels,
    today: chrono::NaiveDate,
}

/// One query, as a row paints it: every string it needs, made once.
///
/// The entry comes along whole because the gestures on a row — replay, copy,
/// forget — are about the query and not about its rendering.
struct Shown {
    entry: Entry,
    headline: SharedString,
    summary: SharedString,
    time: SharedString,
    sql: SharedString,
    multiline: bool,
    /// `×3`, when it has been run more than once.
    runs: Option<SharedString>,
}

impl Shown {
    fn of(entry: &Entry) -> Self {
        Self {
            headline: SharedString::from(entry.headline().to_string()),
            summary: SharedString::from(summary(entry)),
            time: SharedString::from(crate::ui::sql_history::time_of(entry.at)),
            sql: SharedString::from(entry.sql.clone()),
            multiline: entry.is_multiline(),
            runs: (entry.runs > 1).then(|| SharedString::from(format!("×{}", entry.runs))),
            entry: entry.clone(),
        }
    }
}

/// What the theme gives a row, read once per frame and not per row.
#[derive(Clone, Copy)]
struct Look {
    /// A history row is two storeys — the query, then what it did — and it is
    /// what says whether a query is worth recalling.
    row: gpui::Pixels,
    day: gpui::Pixels,
    muted: gpui::Hsla,
    accent: gpui::Hsla,
    danger: gpui::Hsla,
    text: gpui::Hsla,
}

impl Look {
    fn of(cx: &App) -> Self {
        let row = crate::ui::theme::row_height(cx);
        Self {
            row: row * 2.,
            day: row,
            muted: cx.theme().muted_foreground,
            accent: cx.theme().accent,
            danger: cx.theme().danger,
            text: cx.theme().foreground,
        }
    }
}

fn render_day(day: &Day, look: &Look) -> gpui::AnyElement {
    let label: SharedString = match day {
        Day::Today => tr!("sql-history-today"),
        Day::Yesterday => tr!("sql-history-yesterday"),
        Day::On(date) => date.clone().into(),
    };
    h_flex()
        .h(look.day)
        .w_full()
        .px_1()
        .items_center()
        .child(
            div()
                .text_xs()
                .text_color(look.muted)
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(label),
        )
        .into_any_element()
}

/// One query.
///
/// Two storeys, and each says something the other cannot: the query itself, and
/// what it answered — a row count, a duration, an error. A history that only
/// said "SELECT * FROM users" three times over would not tell which of the
/// three is the one worth recalling.
///
/// It is handed the whole prepared list and an index into it rather than an
/// entry: the two gestures below need one that outlives the frame, and cloning
/// it twice per row per frame is what the preparation exists to avoid.
fn render_entry(
    index: usize,
    shown: &Rc<Vec<Shown>>,
    at: usize,
    // The query the console is showing, decided by the caller: reading the
    // application from here would read it while it is being updated.
    current: bool,
    look: &Look,
    entity: &Entity<ClaudhubApp>,
    cx: &App,
) -> gpui::AnyElement {
    let Some(row) = shown.get(at) else {
        return div().into_any_element();
    };
    let mono = cx.theme().mono_font_family.clone();
    let (open, menu) = (entity.clone(), entity.clone());
    let (for_open, for_menu) = (shown.clone(), shown.clone());

    v_flex()
        .id(("sql-history-row", index))
        .h(look.row)
        .w_full()
        .pl_1p5()
        .pr(crate::ui::theme::scroll_gutter())
        .py_0p5()
        .justify_center()
        .cursor_pointer()
        .when(current, |el| el.bg(look.accent.opacity(0.35)))
        .hover(|s| s.bg(look.accent.opacity(0.4)))
        .on_click(move |event, window, cx| {
            let run = event.click_count() > 1;
            let Some(entry) = for_open.get(at).map(|row| row.entry.clone()) else {
                return;
            };
            open.update(cx, |this, cx| {
                this.replay_sql_query(&entry, run, window, cx)
            });
        })
        .child(
            h_flex()
                .w_full()
                .gap_1()
                .items_center()
                .when(!row.entry.ok, |el| {
                    el.child(icon("triangle-alert").xsmall().text_color(look.danger))
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .font_family(mono)
                        .text_color(if row.entry.ok { look.text } else { look.danger })
                        .child(row.headline.clone()),
                )
                // The query goes on past the line shown: an ellipsis, because a
                // one-line row and a twenty-line query look the same otherwise,
                // and it is the tooltip that carries the rest.
                .when(row.multiline, |el| {
                    el.child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(look.muted)
                            .child("⋯"),
                    )
                })
                // A query run again is one row saying so, never several: the
                // count is what the deduplication buys.
                .when_some(row.runs.clone(), |el, runs| {
                    el.child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(look.muted)
                            .child(runs),
                    )
                }),
        )
        .child(
            h_flex()
                .w_full()
                .gap_1()
                .items_center()
                .text_xs()
                .text_color(look.muted)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(row.summary.clone()),
                )
                .child(div().flex_none().child(row.time.clone())),
        )
        .tooltip({
            let sql = row.sql.clone();
            move |window, cx| gpui_component::tooltip::Tooltip::new(sql.clone()).build(window, cx)
        })
        .context_menu(move |popup, _window, _cx| row_menu(popup, &menu, &for_menu, at))
        .into_any_element()
}

/// The second storey: where it ran, and what it answered.
fn summary(entry: &Entry) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(database) = entry.database.as_deref() {
        parts.push(database.to_string());
    } else if !entry.label.is_empty() {
        parts.push(entry.label.clone());
    }
    match (entry.ok, entry.rows, entry.affected, entry.error.as_deref()) {
        (false, _, _, Some(error)) => parts.push(error.to_string()),
        (false, _, _, None) => parts.push(tr!("db-failed").to_string()),
        (true, Some(rows), _, _) => parts.push(tr!("sql-history-rows", { n: rows }).to_string()),
        (true, None, Some(affected), _) => {
            parts.push(tr!("sql-history-affected", { n: affected }).to_string())
        }
        (true, None, None, _) => {}
    }
    if entry.ok {
        parts.push(format!("{} ms", entry.elapsed_ms));
    }
    parts.join(" · ")
}

fn row_menu(
    popup: PopupMenu,
    entity: &Entity<ClaudhubApp>,
    shown: &Rc<Vec<Shown>>,
    at: usize,
) -> PopupMenu {
    let popup = popup.item({
        let (entity, shown) = (entity.clone(), shown.clone());
        PopupMenuItem::new(tr!("sql-history-run"))
            .icon(icon("play"))
            .on_click(move |_, window, cx| {
                let Some(entry) = shown.get(at).map(|row| row.entry.clone()) else {
                    return;
                };
                entity.update(cx, |this, cx| {
                    this.replay_sql_query(&entry, true, window, cx)
                });
            })
    });
    let popup = popup.item({
        let (entity, shown) = (entity.clone(), shown.clone());
        PopupMenuItem::new(tr!("sql-history-copy"))
            .icon(icon("copy"))
            .on_click(move |_, _window, cx| {
                let Some(sql) = shown.get(at).map(|row| row.entry.sql.clone()) else {
                    return;
                };
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(sql));
                entity.update(cx, |this, cx| {
                    this.announce(tr!("copied"), cx);
                });
            })
    });
    popup.item({
        let (entity, shown) = (entity.clone(), shown.clone());
        PopupMenuItem::new(tr!("sql-history-forget"))
            .icon(icon("trash-2"))
            .on_click(move |_, _window, cx| {
                let Some(entry) = shown.get(at).map(|row| row.entry.clone()) else {
                    return;
                };
                entity.update(cx, |this, cx| this.forget_sql_query(&entry, cx));
            })
    })
}

/// Nothing to show: a search that found nothing, or a history not yet written.
///
/// Two messages and not one: "no query yet" in front of a search that has just
/// failed would read as a history that lost everything.
fn empty(query: &str, cx: &App) -> gpui::AnyElement {
    let message = if query.trim().is_empty() {
        tr!("sql-history-empty")
    } else {
        tr!("find-no-match")
    };
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .text_color(cx.theme().muted_foreground)
        .child(icon("clock"))
        .child(div().text_sm().px_4().child(message))
        .into_any_element()
}
