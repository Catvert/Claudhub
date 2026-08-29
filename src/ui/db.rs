//! The "Databases" panel: a schema explorer.
//!
//! Four levels — connection, database, table, column — unfolded **on demand**: a
//! development server carries dozens of databases and each of them hundreds of
//! tables, and loading everything when the panel opens would make every startup
//! pay for something only a corner of which is ever looked at. It is PhpStorm's
//! explorer, and the gesture is the same: you unfold what you are looking for.
//!
//! This panel **never modifies anything**: it reads schemas. What writes is the
//! SQL console beside it (`ui::db_query`), and only within the limits the
//! connection account allows.
//!
//! The connections come from the settings, like the agent profiles: it is the
//! second level of the extension system described in CLAUDE.md — a declaration,
//! not code.

use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;

use gpui::{div, prelude::*, px, uniform_list, Context, Entity, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    menu::{ContextMenuExt, PopupMenu, PopupMenuItem},
    resizable::{resizable_panel, v_resizable},
    v_flex, ActiveTheme, Disableable, Selectable, Sizable,
};

use crate::db;
use crate::runtime::Cmd;
use crate::tr;
use crate::ui::app::ClaudhubApp;
use crate::ui::find::Pane;
use crate::ui::icons::icon;
use crate::ui::settings::Settings;

/// The width of one indentation level, and of the rule marking it. The same as
/// the project explorer's: they are two trees side by side.
const INDENT: f32 = 12.;

/// What is known about a read: nothing, it has gone out, it has arrived, it has
/// failed.
///
/// Four states and not an `Option<Result<…>>`: "not asked yet" and "under way"
/// draw differently — an empty node and a spinner — and confusing them makes the
/// command restart on every frame.
#[derive(Debug)]
pub enum Load<T> {
    Idle,
    Loading,
    Ready(T),
    Failed(String),
}

impl<T> Load<T> {
    fn ready(&self) -> Option<&T> {
        match self {
            Load::Ready(value) => Some(value),
            _ => None,
        }
    }

    /// A read that did not succeed is restarted; a read under way is not.
    fn needs_loading(&self) -> bool {
        matches!(self, Load::Idle | Load::Failed(_))
    }
}

pub struct ConnectionState {
    pub config: db::Connection,
    /// The connection's identity, password excluded: it is by this that the
    /// workers' answers find their place again.
    pub key: String,
    pub expanded: bool,
    pub databases: Load<Vec<DatabaseState>>,
}

pub struct DatabaseState {
    pub info: db::Database,
    pub expanded: bool,
    pub tables: Load<Vec<TableState>>,
}

pub struct TableState {
    pub info: db::Table,
    pub expanded: bool,
    pub columns: Load<Vec<db::Column>>,
}

/// A displayed row.
///
/// **Indices** and not values: the same column appears under its unfolded table
/// and in a filter's result, and a tree of ten thousand entries would otherwise
/// make that many string clones on every rebuild. It is the reason that already
/// holds for `ui::tree`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Connection {
        connection: usize,
    },
    Database {
        connection: usize,
        database: usize,
    },
    Table {
        connection: usize,
        database: usize,
        table: usize,
    },
    Column {
        connection: usize,
        database: usize,
        table: usize,
        column: usize,
    },
    /// A row saying what is happening: a load under way, an error.
    Status {
        depth: usize,
        loading: bool,
        message: SharedString,
    },
}

impl Entry {
    fn depth(&self) -> usize {
        match self {
            Entry::Connection { .. } => 0,
            Entry::Database { .. } => 1,
            Entry::Table { .. } => 2,
            Entry::Column { .. } => 3,
            Entry::Status { depth, .. } => *depth,
        }
    }
}

/// The panel's state.
pub struct DbState {
    pub connections: Vec<ConnectionState>,
    /// The displayed rows, rebuilt on every state change, never at render time:
    /// `uniform_list`'s closure runs for every visible row on every frame.
    ///
    /// Behind an `Rc` because the list's closure needs to own them: cloning the
    /// vector at render time is one copy of ten thousand entries per frame.
    pub entries: Rc<Vec<Entry>>,
    /// The search query `entries` was built for.
    pub query: String,
    /// The row under the cursor, by index into `entries`.
    ///
    /// An index and not an identity — unlike the project explorer, whose tree is
    /// rebuilt under the cursor on every keystroke: here the rebuild is rare,
    /// and an index is enough.
    pub cursor: Option<usize>,
    /// The connections an "index everything" is walking through.
    pub indexing: HashSet<String>,
    /// Show only the databases belonging to the selected checkout.
    ///
    /// True by default, and it costs nothing where no connection declares a
    /// scope — `db::scope::allows` keeps everything when no pattern applies.
    /// In memory and not in the settings: it is a reading posture, like the
    /// journal's level, that changes several times while looking for something.
    pub scoped: bool,
    /// How many databases the scope is hiding, as of the last rebuild.
    pub hidden: usize,
}

impl Default for DbState {
    fn default() -> Self {
        Self {
            connections: Vec::new(),
            entries: Rc::new(Vec::new()),
            query: String::new(),
            cursor: None,
            indexing: HashSet::new(),
            scoped: true,
            hidden: 0,
        }
    }
}

impl ClaudhubApp {
    /// Brings the connection list into line with the settings.
    ///
    /// Called at render time, like everything that depends on a setting. A
    /// connection's state is **picked up by its key**: fixing a password or
    /// renaming a connection must not close the tree just unfolded.
    pub(super) fn sync_db_connections(&mut self, cx: &mut Context<Self>) {
        let wanted: Vec<db::Connection> = Settings::global(cx)
            .databases
            .iter()
            .filter(|connection| connection.is_usable())
            .cloned()
            .collect();
        let same = wanted.len() == self.db.connections.len()
            && wanted
                .iter()
                .zip(self.db.connections.iter())
                .all(|(config, state)| config == &state.config);
        if same {
            return;
        }
        let mut previous = std::mem::take(&mut self.db.connections);
        self.db.connections = wanted
            .into_iter()
            .map(|config| {
                let key = config.key();
                match previous.iter().position(|state| state.key == key) {
                    Some(index) => {
                        let mut state = previous.remove(index);
                        state.config = config;
                        state
                    }
                    None => ConnectionState {
                        key,
                        config,
                        expanded: false,
                        databases: Load::Idle,
                    },
                }
            })
            .collect();
        // The index keys would outlive the connection they name.
        self.db
            .indexing
            .retain(|key| self.db.connections.iter().any(|state| &state.key == key));
        self.db_rebuild(cx);
    }

    fn connection_at(&self, index: usize) -> Option<&ConnectionState> {
        self.db.connections.get(index)
    }

    fn database_at(&self, connection: usize, database: usize) -> Option<&DatabaseState> {
        self.connection_at(connection)?
            .databases
            .ready()?
            .get(database)
    }

    fn table_at(&self, connection: usize, database: usize, table: usize) -> Option<&TableState> {
        self.database_at(connection, database)?
            .tables
            .ready()?
            .get(table)
    }

    fn database_mut(&mut self, connection: usize, database: usize) -> Option<&mut DatabaseState> {
        match &mut self.db.connections.get_mut(connection)?.databases {
            Load::Ready(databases) => databases.get_mut(database),
            _ => None,
        }
    }

    fn table_mut(
        &mut self,
        connection: usize,
        database: usize,
        table: usize,
    ) -> Option<&mut TableState> {
        match &mut self.database_mut(connection, database)?.tables {
            Load::Ready(tables) => tables.get_mut(table),
            _ => None,
        }
    }

    /// The index of the connection carrying this key.
    fn connection_by_key(&self, key: &str) -> Option<usize> {
        self.db
            .connections
            .iter()
            .position(|state| state.key == key)
    }

    // — Loads       ————————————————————————————————————————————————

    fn db_load_databases(&mut self, connection: usize, cx: &mut Context<Self>) {
        let Some(state) = self.db.connections.get_mut(connection) else {
            return;
        };
        state.databases = Load::Loading;
        let config = state.config.clone();
        self.git.send(Cmd::DbDatabases { connection: config });
        self.db_rebuild(cx);
    }

    fn db_load_tables(&mut self, connection: usize, database: usize, cx: &mut Context<Self>) {
        let Some(config) = self.connection_at(connection).map(|s| s.config.clone()) else {
            return;
        };
        let Some(state) = self.database_mut(connection, database) else {
            return;
        };
        let name = state.info.name.clone();
        state.tables = Load::Loading;
        self.git.send(Cmd::DbTables {
            connection: config,
            database: name,
        });
        self.db_rebuild(cx);
    }

    fn db_load_columns(
        &mut self,
        connection: usize,
        database: usize,
        table: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(config) = self.connection_at(connection).map(|s| s.config.clone()) else {
            return;
        };
        let Some(database_name) = self
            .database_at(connection, database)
            .map(|d| d.info.name.clone())
        else {
            return;
        };
        let Some(state) = self.table_mut(connection, database, table) else {
            return;
        };
        let name = state.info.name.clone();
        state.columns = Load::Loading;
        self.git.send(Cmd::DbColumns {
            connection: config,
            database: database_name,
            table: name,
        });
        self.db_rebuild(cx);
    }

    /// Loads the columns of **every** table of a database.
    ///
    /// One command and not one per table: that is what makes the filter and the
    /// completions affordable on a three-hundred-table schema.
    fn db_load_all_columns(&mut self, connection: usize, database: usize, cx: &mut Context<Self>) {
        let Some(config) = self.connection_at(connection).map(|s| s.config.clone()) else {
            return;
        };
        let Some(state) = self.database_mut(connection, database) else {
            return;
        };
        let name = state.info.name.clone();
        if let Load::Ready(tables) = &mut state.tables {
            for table in tables.iter_mut() {
                if table.columns.needs_loading() {
                    table.columns = Load::Loading;
                }
            }
        }
        self.git.send(Cmd::DbAllColumns {
            connection: config,
            database: name,
        });
        self.db_rebuild(cx);
    }

    // — Arrivals ———————————————————————————————————————————————————

    pub(super) fn db_databases_arrived(
        &mut self,
        key: String,
        databases: crate::runtime::protocol::DbResult<Vec<db::Database>>,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.connection_by_key(&key) else {
            return; // the connection was removed while waiting
        };
        let Some(state) = self.db.connections.get_mut(index) else {
            return;
        };
        state.databases = match databases {
            Ok(list) => Load::Ready(
                list.into_iter()
                    .map(|info| DatabaseState {
                        info,
                        expanded: false,
                        tables: Load::Idle,
                    })
                    .collect(),
            ),
            Err(message) => Load::Failed(message),
        };
        self.db_continue_indexing(index, cx);
        self.db_rebuild(cx);
    }

    pub(super) fn db_tables_arrived(
        &mut self,
        key: String,
        database: String,
        tables: crate::runtime::protocol::DbResult<Vec<db::Table>>,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.connection_by_key(&key) else {
            return;
        };
        let Some(position) = self.database_position(index, &database) else {
            return;
        };
        if let Some(state) = self.database_mut(index, position) {
            state.tables = match tables {
                Ok(list) => Load::Ready(
                    list.into_iter()
                        .map(|info| TableState {
                            info,
                            expanded: false,
                            columns: Load::Idle,
                        })
                        .collect(),
                ),
                Err(message) => Load::Failed(message),
            };
        }
        self.db_continue_indexing(index, cx);
        self.db_rebuild(cx);
    }

    pub(super) fn db_columns_arrived(
        &mut self,
        key: String,
        database: String,
        table: String,
        columns: crate::runtime::protocol::DbResult<Vec<db::Column>>,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.connection_by_key(&key) else {
            return;
        };
        let Some(position) = self.database_position(index, &database) else {
            return;
        };
        let Some(table_position) = self.table_position(index, position, &table) else {
            return;
        };
        if let Some(state) = self.table_mut(index, position, table_position) {
            state.columns = match columns {
                Ok(list) => Load::Ready(list),
                Err(message) => Load::Failed(message),
            };
        }
        self.db_continue_indexing(index, cx);
        self.db_rebuild(cx);
    }

    pub(super) fn db_all_columns_arrived(
        &mut self,
        key: String,
        database: String,
        columns: crate::runtime::protocol::DbResult<BTreeMap<String, Vec<db::Column>>>,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.connection_by_key(&key) else {
            return;
        };
        // The SQL console completes on what the panel has indexed: it therefore
        // benefits from the same read, without starting a second one.
        if let Ok(indexed) = &columns {
            self.db_schema_indexed(&key, &database, indexed, cx);
        }
        let Some(position) = self.database_position(index, &database) else {
            return;
        };
        if let Some(state) = self.database_mut(index, position) {
            if let Load::Ready(tables) = &mut state.tables {
                match columns {
                    Ok(mut indexed) => {
                        for table in tables.iter_mut() {
                            if matches!(table.columns, Load::Loading) {
                                // A table missing from the batch vanished
                                // between the request and the answer: marking it
                                // loaded avoids asking for it on every frame.
                                table.columns = Load::Ready(
                                    indexed.remove(&table.info.name).unwrap_or_default(),
                                );
                            }
                        }
                    }
                    Err(message) => {
                        for table in tables.iter_mut() {
                            if matches!(table.columns, Load::Loading) {
                                table.columns = Load::Failed(message.clone());
                            }
                        }
                    }
                }
            }
        }
        self.db_continue_indexing(index, cx);
        self.db_rebuild(cx);
    }

    fn database_position(&self, connection: usize, name: &str) -> Option<usize> {
        self.connection_at(connection)?
            .databases
            .ready()?
            .iter()
            .position(|state| state.info.name == name)
    }

    fn table_position(&self, connection: usize, database: usize, name: &str) -> Option<usize> {
        self.database_at(connection, database)?
            .tables
            .ready()?
            .iter()
            .position(|state| state.info.name == name)
    }

    // — The tree ————————————————————————————————————————————————————

    /// Rebuilds the displayed rows.
    /// What the selected checkout is called, as a scope pattern names it.
    ///
    /// The slug is **the linked worktree's folder name, and nothing on the main
    /// checkout**: `wt` puts a worktree in `<root>/<slug>`, and a pattern that
    /// names a slug is not about the main repository — see `db::scope`.
    fn db_scope_vars(&self) -> db::scope::Vars {
        let Some(worktree) = self.active_worktree() else {
            return db::scope::Vars::default();
        };
        let name = worktree.label();
        db::scope::Vars {
            worktree: Some(name.clone()),
            slug: (!worktree.is_main).then_some(name),
            branch: worktree.branch.clone(),
        }
    }

    /// The patterns in force for a connection, empty when nothing filters.
    fn db_scope(&self, connection: usize) -> Vec<String> {
        if !self.db.scoped {
            return Vec::new();
        }
        let Some(state) = self.connection_at(connection) else {
            return Vec::new();
        };
        db::scope::expand(&state.config.scope, &self.db_scope_vars())
    }

    /// Does this database belong to the checkout being looked at?
    fn db_in_scope(&self, connection: usize, database: usize) -> bool {
        let Some(state) = self.database_at(connection, database) else {
            return true;
        };
        db::scope::allows(&self.db_scope(connection), &state.info.name)
    }

    /// How many databases the scope is hiding, all connections together.
    ///
    /// The bar says it: a filter that removes seventy-eight databases without a
    /// word reads as a broken connection, and the whole point of a scope is that
    /// one knows it is on. Read from `DbState::hidden`, which `db_rebuild`
    /// fills — this walk is not for a render.
    fn db_hidden_count(&self) -> usize {
        (0..self.db.connections.len())
            .map(|connection| {
                let patterns = self.db_scope(connection);
                if patterns.is_empty() {
                    return 0;
                }
                self.connection_at(connection)
                    .and_then(|state| state.databases.ready())
                    .map(|databases| {
                        databases
                            .iter()
                            .filter(|state| !db::scope::allows(&patterns, &state.info.name))
                            .count()
                    })
                    .unwrap_or(0)
            })
            .sum()
    }

    /// Shows everything, or only the checkout's databases.
    pub(super) fn db_toggle_scope(&mut self, cx: &mut Context<Self>) {
        self.db.scoped = !self.db.scoped;
        self.db_rebuild(cx);
    }

    pub(super) fn db_rebuild(&mut self, cx: &mut Context<Self>) {
        // The row under the cursor is followed by its value, not by its index:
        // unfolding inserts rows above it.
        let previous = self
            .db
            .cursor
            .and_then(|index| self.db.entries.get(index).cloned());
        let query = self.db.query.clone();
        self.db.entries = Rc::new(if query.trim().is_empty() {
            self.db_expanded_entries()
        } else {
            self.db_filtered_entries(&query)
        });
        // What the scope hides is counted here and not in the bar: the count
        // expands the patterns and lowercases them against every database, and
        // the bar is drawn on every frame. It is the same reading as the rows,
        // so what invalidates one invalidates the other.
        self.db.hidden = self.db_hidden_count();
        self.db.cursor = previous
            .and_then(|entry| self.db.entries.iter().position(|other| *other == entry))
            .or_else(|| {
                self.db
                    .cursor
                    .map(|index| index.min(self.db.entries.len().saturating_sub(1)))
                    .filter(|_| !self.db.entries.is_empty())
            });
        cx.notify();
    }

    fn db_expanded_entries(&self) -> Vec<Entry> {
        let mut entries = Vec::new();
        for (connection, state) in self.db.connections.iter().enumerate() {
            entries.push(Entry::Connection { connection });
            if !state.expanded {
                continue;
            }
            match &state.databases {
                Load::Idle => {}
                Load::Loading => entries.push(status(1, true, tr!("db-connecting"))),
                Load::Failed(message) => entries.push(status(1, false, message.clone().into())),
                Load::Ready(databases) => {
                    let patterns = self.db_scope(connection);
                    for (database, state) in databases.iter().enumerate() {
                        // Out of the checkout's scope: skipped, never removed
                        // from the state — the indices an entry carries are its
                        // place in that vector.
                        if !db::scope::allows(&patterns, &state.info.name) {
                            continue;
                        }
                        entries.push(Entry::Database {
                            connection,
                            database,
                        });
                        if !state.expanded {
                            continue;
                        }
                        match &state.tables {
                            Load::Idle => {}
                            Load::Loading => {
                                entries.push(status(2, true, tr!("db-loading-tables")))
                            }
                            Load::Failed(message) => {
                                entries.push(status(2, false, message.clone().into()))
                            }
                            Load::Ready(tables) => {
                                for (table, state) in tables.iter().enumerate() {
                                    entries.push(Entry::Table {
                                        connection,
                                        database,
                                        table,
                                    });
                                    if !state.expanded {
                                        continue;
                                    }
                                    match &state.columns {
                                        Load::Idle => {}
                                        Load::Loading => {
                                            entries.push(status(3, true, tr!("db-loading-columns")))
                                        }
                                        Load::Failed(message) => {
                                            entries.push(status(3, false, message.clone().into()))
                                        }
                                        Load::Ready(columns) => {
                                            for column in 0..columns.len() {
                                                entries.push(Entry::Column {
                                                    connection,
                                                    database,
                                                    table,
                                                    column,
                                                });
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        entries
    }

    /// The filtered tree: the tables and columns whose name matches, with their
    /// ancestors, **ignoring collapses**.
    ///
    /// A result hidden inside a collapsed node would not be visible, and the
    /// search would look as if it had found nothing — it is the rule the
    /// explorer and the review list already follow.
    fn db_filtered_entries(&self, query: &str) -> Vec<Entry> {
        let hit = |name: &str| crate::ui::find::matches(query, name);
        let mut entries = Vec::new();
        for (connection, state) in self.db.connections.iter().enumerate() {
            let mut children = Vec::new();
            match &state.databases {
                Load::Loading => children.push(status(1, true, tr!("db-connecting"))),
                Load::Ready(databases) => {
                    let patterns = self.db_scope(connection);
                    for (database, state) in databases.iter().enumerate() {
                        // A search does not reach past the scope: what is not
                        // this checkout's is not among what one is looking for,
                        // and the bar says how much that is.
                        if !db::scope::allows(&patterns, &state.info.name) {
                            continue;
                        }
                        let mut rows = Vec::new();
                        match &state.tables {
                            Load::Loading => rows.push(status(2, true, tr!("db-loading-tables"))),
                            Load::Ready(tables) => {
                                let mut indexing = false;
                                for (table, state) in tables.iter().enumerate() {
                                    let mut columns = Vec::new();
                                    match &state.columns {
                                        Load::Ready(list) => {
                                            for (column, info) in list.iter().enumerate() {
                                                if hit(&info.name) {
                                                    columns.push(Entry::Column {
                                                        connection,
                                                        database,
                                                        table,
                                                        column,
                                                    });
                                                }
                                            }
                                        }
                                        Load::Loading => indexing = true,
                                        _ => {}
                                    }
                                    if hit(&state.info.name) || !columns.is_empty() {
                                        rows.push(Entry::Table {
                                            connection,
                                            database,
                                            table,
                                        });
                                        rows.append(&mut columns);
                                    }
                                }
                                if indexing {
                                    rows.push(status(2, true, tr!("db-indexing")));
                                }
                            }
                            _ => {}
                        }
                        if !rows.is_empty() || hit(&state.info.name) {
                            children.push(Entry::Database {
                                connection,
                                database,
                            });
                            children.append(&mut rows);
                        }
                    }
                }
                _ => {}
            }
            if !children.is_empty() || hit(&state.config.label()) {
                entries.push(Entry::Connection { connection });
                entries.append(&mut children);
            }
        }
        entries
    }

    /// Starts what is needed for the filter to see the whole tree.
    ///
    /// **Connections never unfolded are left alone**: typing three letters into
    /// a search field must not open a connection to a production server. What
    /// this walk completes is what is already open; "index everything" is the
    /// gesture that connects everywhere, and it is explicit.
    fn db_index_for_filter(&mut self, cx: &mut Context<Self>) {
        for connection in 0..self.db.connections.len() {
            let Some(state) = self.connection_at(connection) else {
                continue;
            };
            let Some(databases) = state.databases.ready() else {
                continue;
            };
            for database in 0..databases.len() {
                if !self.db_in_scope(connection, database) {
                    continue;
                }
                let Some(state) = self.database_at(connection, database) else {
                    break;
                };
                match &state.tables {
                    Load::Idle => self.db_load_tables(connection, database, cx),
                    Load::Ready(tables)
                        if tables.iter().any(|table| table.columns.needs_loading()) =>
                    {
                        self.db_load_all_columns(connection, database, cx)
                    }
                    _ => {}
                }
            }
        }
    }

    /// Indexes everything, including what has never been unfolded.
    ///
    /// Unlike the filter's implicit indexing, this one **connects everywhere, on
    /// purpose**: it is what one asks for when the search and the completions
    /// should cover the whole schema.
    pub(super) fn db_index_all(&mut self, cx: &mut Context<Self>) {
        for connection in 0..self.db.connections.len() {
            let Some(state) = self.db.connections.get(connection) else {
                continue;
            };
            self.db.indexing.insert(state.key.clone());
            // An explicit request retries what had failed; the continuation, for
            // its part, never retries — that would be a loop.
            if state.databases.needs_loading() {
                self.db_load_databases(connection, cx);
            } else if state.databases.ready().is_some() {
                self.db_continue_indexing(connection, cx);
            }
        }
        cx.notify();
    }

    /// Advances a connection's indexing on every read that arrives, until there
    /// is nothing left to ask for.
    fn db_continue_indexing(&mut self, connection: usize, cx: &mut Context<Self>) {
        let Some(key) = self.connection_at(connection).map(|s| s.key.clone()) else {
            return;
        };
        if !self.db.indexing.contains(&key) {
            return;
        }
        let mut pending = false;
        match self.connection_at(connection).map(|s| &s.databases) {
            Some(Load::Loading) => pending = true,
            Some(Load::Ready(databases)) => {
                for database in 0..databases.len() {
                    // Indexing follows the scope: connecting to the eighty
                    // databases a scope has just hidden is exactly what one
                    // asked it not to do.
                    if !self.db_in_scope(connection, database) {
                        continue;
                    }
                    let Some(state) = self.database_at(connection, database) else {
                        break;
                    };
                    match &state.tables {
                        Load::Idle => {
                            pending = true;
                            self.db_load_tables(connection, database, cx);
                        }
                        Load::Loading => pending = true,
                        Load::Failed(_) => {}
                        Load::Ready(tables) => {
                            if tables.iter().any(|table| table.columns.needs_loading()) {
                                pending = true;
                                self.db_load_all_columns(connection, database, cx);
                            } else if tables
                                .iter()
                                .any(|table| matches!(table.columns, Load::Loading))
                            {
                                pending = true;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        if !pending {
            self.db.indexing.remove(&key);
        }
    }

    /// Forgets what was known of a connection, and re-reads it if it is open.
    pub(super) fn db_refresh(&mut self, connection: Option<usize>, cx: &mut Context<Self>) {
        let targets: Vec<usize> = match connection {
            Some(index) => vec![index],
            None => (0..self.db.connections.len()).collect(),
        };
        for index in targets {
            let Some(state) = self.db.connections.get_mut(index) else {
                continue;
            };
            state.databases = Load::Idle;
            if state.expanded {
                self.db_load_databases(index, cx);
            }
        }
        self.db_rebuild(cx);
    }

    /// Unfolds or collapses the row, and starts the missing read.
    pub(super) fn db_toggle(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.db.entries.get(index).cloned() else {
            return;
        };
        self.db.cursor = Some(index);
        match entry {
            Entry::Connection { connection } => {
                let Some(state) = self.db.connections.get_mut(connection) else {
                    return;
                };
                state.expanded = !state.expanded;
                let load = state.expanded && state.databases.needs_loading();
                if load {
                    self.db_load_databases(connection, cx);
                }
            }
            Entry::Database {
                connection,
                database,
            } => {
                let Some(state) = self.database_mut(connection, database) else {
                    return;
                };
                state.expanded = !state.expanded;
                let load = state.expanded && state.tables.needs_loading();
                if load {
                    self.db_load_tables(connection, database, cx);
                }
            }
            Entry::Table {
                connection,
                database,
                table,
            } => {
                let Some(state) = self.table_mut(connection, database, table) else {
                    return;
                };
                state.expanded = !state.expanded;
                let load = state.expanded && state.columns.needs_loading();
                if load {
                    self.db_load_columns(connection, database, table, cx);
                }
            }
            Entry::Column { .. } | Entry::Status { .. } => {}
        }
        self.db_rebuild(cx);
    }

    fn db_expanded(&self, entry: &Entry) -> Option<bool> {
        match *entry {
            Entry::Connection { connection } => Some(self.connection_at(connection)?.expanded),
            Entry::Database {
                connection,
                database,
            } => Some(self.database_at(connection, database)?.expanded),
            Entry::Table {
                connection,
                database,
                table,
            } => Some(self.table_at(connection, database, table)?.expanded),
            Entry::Column { .. } | Entry::Status { .. } => None,
        }
    }

    // — The keyboard —————————————————————————————————————————————————

    pub(super) fn db_step_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.db.entries.is_empty() {
            return;
        }
        let last = self.db.entries.len() - 1;
        let next = match self.db.cursor {
            Some(index) => (index as isize + delta).clamp(0, last as isize) as usize,
            None => 0,
        };
        self.db.cursor = Some(next);
        self.db_scroll
            .scroll_to_item(next, gpui::ScrollStrategy::Center);
        cx.notify();
    }

    /// Right: unfold, or go down one row. Left: collapse, or go up to the
    /// parent. They are every explorer's gestures.
    pub(super) fn db_fold_cursor(&mut self, open: bool, cx: &mut Context<Self>) {
        let Some(index) = self.db.cursor else { return };
        let Some(entry) = self.db.entries.get(index).cloned() else {
            return;
        };
        match (open, self.db_expanded(&entry)) {
            (true, Some(false)) | (false, Some(true)) => self.db_toggle(index, cx),
            (true, _) => self.db_step_cursor(1, cx),
            (false, _) => {
                let depth = entry.depth();
                if let Some(parent) = self.db.entries[..index]
                    .iter()
                    .rposition(|other| other.depth() < depth)
                {
                    self.db.cursor = Some(parent);
                    self.db_scroll
                        .scroll_to_item(parent, gpui::ScrollStrategy::Center);
                    cx.notify();
                }
            }
        }
    }

    /// Enter: open a console on the row, like a double click.
    pub(super) fn db_open_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.db.cursor else { return };
        let Some(entry) = self.db.entries.get(index).cloned() else {
            return;
        };
        match entry {
            Entry::Connection { .. } | Entry::Database { .. } => self.db_toggle(index, cx),
            Entry::Table { .. } | Entry::Column { .. } => self.open_db_console(
                &entry,
                crate::ui::db_query::ConsoleTarget::Current,
                window,
                cx,
            ),
            Entry::Status { .. } => {}
        }
    }

    /// Opens the SQL console on what a row names.
    ///
    /// A table gives a ready-made `SELECT * FROM …`: it is the first thing one
    /// writes after finding a table, and typing it every time is what makes
    /// people not use an explorer.
    ///
    /// `target` is what tells the click from the menu entry beside it: a click
    /// **reuses** the console one is in, because browsing a schema with the
    /// mouse would otherwise leave ten tabs behind.
    pub(super) fn open_db_console(
        &mut self,
        entry: &Entry,
        target: crate::ui::db_query::ConsoleTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (connection, database, table) = match *entry {
            Entry::Connection { connection } => (connection, None, None),
            Entry::Database {
                connection,
                database,
            } => (
                connection,
                self.database_at(connection, database)
                    .map(|state| state.info.name.clone()),
                None,
            ),
            Entry::Table {
                connection,
                database,
                table,
            }
            | Entry::Column {
                connection,
                database,
                table,
                ..
            } => (
                connection,
                self.database_at(connection, database)
                    .map(|state| state.info.name.clone()),
                self.table_at(connection, database, table)
                    .map(|state| state.info.name.clone()),
            ),
            Entry::Status { .. } => return,
        };
        let Some(config) = self.connection_at(connection).map(|s| s.config.clone()) else {
            return;
        };
        self.start_db_console(target, config, database, table, window, cx);
    }

    // — Rendering ———————————————————————————————————————————————————

    pub(super) fn render_db(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.sync_db_connections(cx);
        let query = self.query(Pane::Db, cx);
        if self.db.query != query {
            self.db.query = query;
            if !self.db.query.trim().is_empty() {
                self.db_index_for_filter(cx);
            }
            self.db_rebuild(cx);
        }
        let find = self.render_find(Pane::Db, cx);
        let bar = self.render_db_bar(cx);
        let vim = Settings::global(cx).vim_mode;
        let focus = self.db_focus.clone();
        let scroll = self.db_scroll.clone();

        if self.db.connections.is_empty() {
            return v_flex()
                .size_full()
                .child(bar)
                .child(
                    v_flex()
                        .size_full()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .text_color(cx.theme().muted_foreground)
                        .child(icon("database"))
                        .child(div().text_sm().px_4().child(tr!("db-empty")))
                        .child(
                            Button::new("db-add-first")
                                .outline()
                                .small()
                                .icon(icon("plus"))
                                .label(tr!("db-add-connection"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_settings_at(
                                        crate::ui::settings_view::Page::Databases,
                                        window,
                                        cx,
                                    )
                                })),
                        ),
                )
                .into_any_element();
        }

        let entries = self.db.entries.clone();
        let look = Look::of(cx);
        let cursor = self.db.cursor;
        let entity = cx.entity();
        let count = entries.len();

        let tree = v_flex().size_full().child(bar).children(find).child(
            div()
                .id("db-tree")
                // The arrows belong to this tree when it has the focus, like
                // the explorer's to its own.
                .key_context(crate::ui::shortcuts::db_context(vim))
                .track_focus(&focus)
                .flex_1()
                .min_h_0()
                .child(
                    self.scrolled(
                        "db-tree-bar",
                        &scroll,
                        crate::ui::motion::Axes::Vertical,
                        window,
                        uniform_list("db-entries", count, move |visible, _window, cx| {
                            visible
                                .map(|index| {
                                    render_row(&entries, index, cursor, &look, &entity, cx)
                                })
                                .collect::<Vec<_>>()
                        })
                        .size_full()
                        .track_scroll(&scroll.clone()),
                        cx,
                    ),
                ),
        );
        // **The queries already run live here**, under the tree they are about:
        // what one does with a past query is run it against a schema one is
        // looking at, and a tab away was one gesture too many for that. The
        // share is adjustable — one unfolds a schema, then reads a hundred rows
        // of history, and no fixed proportion suits both.
        let history = self.render_sql_history(window, cx);
        v_resizable("db-split-history")
            .with_state(&self.db_history_split.clone())
            .child(resizable_panel().child(tree))
            .child(
                resizable_panel()
                    .size(px(220.))
                    .size_range(px(80.)..px(720.))
                    .child(history),
            )
            .into_any_element()
    }

    fn render_db_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let indexing = !self.db.indexing.is_empty();
        let count = self.db.connections.len();
        h_flex()
            .h(crate::ui::theme::bar_height(cx))
            .w_full()
            .px_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(icon("database").xsmall())
            .child(
                div()
                    .flex_1()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr!("db-connections-count", { n: count })),
            )
            .children(self.render_db_scope(cx))
            .child(self.find_button(Pane::Db, cx))
            // One more console, from where the tables are: a second query is
            // almost always a second query about what one is already looking at.
            .child(
                Button::new("db-new-console")
                    .ghost()
                    .xsmall()
                    .icon(icon("file-code"))
                    .tooltip(tr!("db-new-console"))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_another_console(window, cx)),
                    ),
            )
            .child(
                Button::new("db-index-all")
                    .ghost()
                    .xsmall()
                    .icon(icon("zap"))
                    .tooltip(tr!("db-index-all"))
                    .disabled(indexing)
                    .on_click(cx.listener(|this, _, _window, cx| this.db_index_all(cx))),
            )
            .child(
                Button::new("db-refresh")
                    .ghost()
                    .xsmall()
                    .icon(icon("refresh-cw"))
                    .tooltip(tr!("action-refresh"))
                    .on_click(cx.listener(|this, _, _window, cx| this.db_refresh(None, cx))),
            )
            .child(
                Button::new("db-add")
                    .ghost()
                    .xsmall()
                    .icon(icon("plus"))
                    .tooltip(tr!("db-add-connection"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_settings_at(crate::ui::settings_view::Page::Databases, window, cx)
                    })),
            )
    }
}

impl ClaudhubApp {
    /// The scope switch, and what it hides.
    ///
    /// **Only where a scope applies**: a project whose databases are not cloned
    /// per worktree has no pattern, nothing is filtered, and a button that
    /// switches between two identical lists is a button that reads as broken.
    fn render_db_scope(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let declared = self
            .db
            .connections
            .iter()
            .any(|state| !state.config.scope.trim().is_empty());
        if !declared {
            return None;
        }
        let scoped = self.db.scoped;
        let hidden = self.db.hidden;
        Some(
            h_flex()
                .gap_1()
                .items_center()
                .when(scoped && hidden > 0, |el| {
                    el.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(tr!("db-scope-hidden", { n: hidden })),
                    )
                })
                .child(
                    Button::new("db-scope")
                        .ghost()
                        .xsmall()
                        .icon(icon("funnel"))
                        .tooltip(if scoped {
                            tr!("db-scope-on")
                        } else {
                            tr!("db-scope-off")
                        })
                        .selected(scoped)
                        .on_click(cx.listener(|this, _, _window, cx| this.db_toggle_scope(cx))),
                ),
        )
    }
}

fn status(depth: usize, loading: bool, message: SharedString) -> Entry {
    Entry::Status {
        depth,
        loading,
        message,
    }
}

/// What the theme gives a row, read once per frame and not per row.
#[derive(Clone)]
struct Look {
    height: gpui::Pixels,
    muted: gpui::Hsla,
    accent: gpui::Hsla,
    guide: gpui::Hsla,
    danger: gpui::Hsla,
    warning: gpui::Hsla,
    info: gpui::Hsla,
    success: gpui::Hsla,
}

impl Look {
    fn of(cx: &gpui::App) -> Self {
        Self {
            height: crate::ui::theme::row_height(cx),
            muted: cx.theme().muted_foreground,
            accent: cx.theme().accent,
            guide: cx.theme().border.opacity(0.7),
            danger: cx.theme().danger,
            warning: cx.theme().warning,
            info: cx.theme().info,
            success: cx.theme().success,
        }
    }
}

fn indent_guides(depth: usize, look: &Look) -> impl IntoIterator<Item = gpui::Div> + use<> {
    let guide = look.guide;
    (0..depth).map(move |_| {
        div()
            .w(px(INDENT))
            .h_full()
            .flex_none()
            .border_l_1()
            .border_color(guide)
    })
}

/// One row of the tree.
///
/// The state is re-read here, inside the list's closure, and not copied into
/// `Entry`: an entry carries only indices, and a table's name has no business
/// being cloned on every rebuild to be shown for a few frames.
fn render_row(
    entries: &Rc<Vec<Entry>>,
    index: usize,
    cursor: Option<usize>,
    look: &Look,
    entity: &Entity<ClaudhubApp>,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    let Some(entry) = entries.get(index).cloned() else {
        return div().into_any_element();
    };
    let app = entity.read(cx);
    let at_cursor = cursor == Some(index);
    let depth = entry.depth();

    if let Entry::Status {
        loading, message, ..
    } = &entry
    {
        return h_flex()
            .id(("db-status", index))
            .h(look.height)
            .items_center()
            .pl_1()
            .pr_2()
            .children(indent_guides(depth, look))
            .child(div().w(px(14.)).flex_none())
            .child(
                icon(if *loading {
                    "loader-circle"
                } else {
                    "circle-x"
                })
                .xsmall()
                .text_color(if *loading { look.muted } else { look.danger }),
            )
            .child(
                div()
                    .pl_1()
                    .flex_1()
                    .truncate()
                    .text_xs()
                    .text_color(if *loading { look.muted } else { look.danger })
                    .child(message.clone()),
            )
            .into_any_element();
    }

    let Some((glyph, tint, name, detail, has_tooltip)) = describe(app, &entry, look) else {
        return div().into_any_element();
    };
    let expanded = app.db_expanded(&entry);
    let for_menu = entry.clone();
    let (click, menu) = (entity.clone(), entity.clone());

    h_flex()
        .id(("db-row", index))
        .h(look.height)
        .w_full()
        .pl_1()
        .pr(crate::ui::theme::scroll_gutter())
        .items_center()
        .cursor_pointer()
        .when(at_cursor, |el| el.bg(look.accent.opacity(0.5)))
        .hover(|s| s.bg(look.accent.opacity(0.4)))
        .on_click(move |_, window, cx| {
            click.update(cx, |this, cx| {
                // The click takes the focus back: without that, the arrows would
                // go on browsing the project explorer.
                this.db_focus.clone().focus(window, cx);
                this.db_toggle(index, cx);
            });
        })
        .children(indent_guides(depth, look))
        .child(match expanded {
            Some(true) => icon("chevron-down")
                .xsmall()
                .text_color(look.muted)
                .into_any_element(),
            Some(false) => icon("chevron-right")
                .xsmall()
                .text_color(look.muted)
                .into_any_element(),
            // The place of the chevron a column does not have: without it, the
            // names do not line up from one level to the next.
            None => div().w(px(14.)).flex_none().into_any_element(),
        })
        .child(icon(glyph).xsmall().text_color(tint))
        .child(div().pl_1().truncate().text_sm().child(name))
        .when_some(detail, |el, detail| {
            el.child(
                div()
                    .pl_1()
                    .flex_1()
                    .truncate()
                    .text_xs()
                    .text_color(look.muted)
                    .child(detail),
            )
        })
        // **The tooltip's text is written inside the closure**, which gpui calls
        // when the pointer stops on the row — not for every visible row on every
        // frame. It is a `format!`, a `join` and, for a table, a row count and a
        // size to spell out; done at render time it was fifty of them a frame,
        // for one that is ever read.
        .when(has_tooltip, |el| {
            let (tip, for_tip) = (entity.clone(), entry.clone());
            el.tooltip(move |window, cx| {
                let text = tooltip_of(tip.read(cx), &for_tip).unwrap_or_default();
                gpui_component::tooltip::Tooltip::new(text).build(window, cx)
            })
        })
        .context_menu(move |popup, _window, _cx| row_menu(popup, &menu, &for_menu))
        .into_any_element()
}

/// A row's icon, tint, name, detail, and whether it has a tooltip.
///
/// A single function for all four levels: they are the same four things, and
/// separating them would make the same layout four times. The last is a
/// **yes-or-no** and not the text: what a tooltip says is written by
/// `tooltip_of`, once the pointer has stopped — the two go together, and a
/// level that gains a tooltip has to say so in both.
#[allow(clippy::type_complexity)]
fn describe(
    app: &ClaudhubApp,
    entry: &Entry,
    look: &Look,
) -> Option<(
    &'static str,
    gpui::Hsla,
    SharedString,
    Option<SharedString>,
    bool,
)> {
    match *entry {
        Entry::Connection { connection } => {
            let state = app.connection_at(connection)?;
            let tint = match &state.databases {
                Load::Ready(_) => look.success,
                Load::Failed(_) => look.danger,
                _ => look.muted,
            };
            let glyph = match state.config.engine {
                db::Engine::Sqlite => "database",
                db::Engine::Mysql => "globe",
            };
            Some((
                glyph,
                tint,
                state.config.label().into(),
                Some(state.config.detail().into()),
                true,
            ))
        }
        Entry::Database {
            connection,
            database,
        } => {
            let state = app.database_at(connection, database)?;
            Some((
                "database",
                look.muted,
                state.info.name.clone().into(),
                None,
                state.info.charset.is_some() || state.info.collation.is_some(),
            ))
        }
        Entry::Table {
            connection,
            database,
            table,
        } => {
            let state = app.table_at(connection, database, table)?;
            let info = &state.info;
            Some((
                if info.view { "eye" } else { "table" },
                look.muted,
                info.name.clone().into(),
                info.rows.map(|rows| {
                    SharedString::from(format!("{} {}", db::count(rows), tr!("db-rows")))
                }),
                info.view
                    || info.engine.is_some()
                    || info.rows.is_some()
                    || info.bytes.is_some()
                    || info.collation.is_some()
                    || info.comment.is_some(),
            ))
        }
        Entry::Column {
            connection,
            database,
            table,
            column,
        } => {
            let state = app.table_at(connection, database, table)?;
            let info = state.columns.ready()?.get(column)?;
            // The primary key and the foreign key carry the same glyph and two
            // tints: it is the same family — what a row is named by — and two
            // different drawings would say nothing more.
            let (glyph, tint) = if info.primary_key {
                ("tag", look.warning)
            } else if info.foreign_key.is_some() {
                ("tag", look.info)
            } else {
                ("columns-2", look.muted)
            };
            Some((
                glyph,
                tint,
                info.name.clone().into(),
                Some(info.data_type.clone().into()),
                true,
            ))
        }
        Entry::Status { .. } => None,
    }
}

/// What a row's tooltip says: everything the line has no room for.
///
/// Called when the pointer stops on the row, never at render time — see the
/// closure in `render_row`. `describe` says which rows have one, and that
/// answer must agree with the `None`s here.
fn tooltip_of(app: &ClaudhubApp, entry: &Entry) -> Option<SharedString> {
    match *entry {
        Entry::Connection { connection } => {
            let state = app.connection_at(connection)?;
            Some(
                format!(
                    "{} · {}",
                    state.config.engine.label(),
                    state.config.detail()
                )
                .into(),
            )
        }
        Entry::Database {
            connection,
            database,
        } => {
            let state = app.database_at(connection, database)?;
            let mut parts = Vec::new();
            parts.extend(state.info.charset.clone());
            parts.extend(state.info.collation.clone());
            (!parts.is_empty()).then(|| parts.join(" · ").into())
        }
        Entry::Table {
            connection,
            database,
            table,
        } => {
            let info = &app.table_at(connection, database, table)?.info;
            let mut parts = Vec::new();
            if info.view {
                parts.push(tr!("db-view").to_string());
            }
            parts.extend(info.engine.clone());
            if let Some(rows) = info.rows {
                parts.push(format!("~{} {}", db::count(rows), tr!("db-rows")));
            }
            parts.extend(info.bytes.map(db::size));
            parts.extend(info.collation.clone());
            parts.extend(info.comment.clone());
            (!parts.is_empty()).then(|| parts.join(" · ").into())
        }
        Entry::Column {
            connection,
            database,
            table,
            column,
        } => {
            let state = app.table_at(connection, database, table)?;
            let info = state.columns.ready()?.get(column)?;
            let mut parts = vec![info.data_type.clone()];
            parts.push(
                if info.nullable {
                    tr!("db-nullable")
                } else {
                    tr!("db-not-null")
                }
                .to_string(),
            );
            if let Some(default) = &info.default {
                parts.push(format!("{} {default}", tr!("db-default")));
            }
            if info.primary_key {
                parts.push(tr!("db-primary-key").to_string());
            }
            if info.unique {
                parts.push(tr!("db-unique").to_string());
            }
            if info.auto_increment {
                parts.push(tr!("db-auto-increment").to_string());
            }
            if let Some(target) = &info.foreign_key {
                parts.push(format!("{} {target}", tr!("db-references")));
            }
            parts.extend(info.charset.clone());
            parts.extend(info.collation.clone());
            parts.extend(info.comment.clone());
            Some(parts.join(" · ").into())
        }
        Entry::Status { .. } => None,
    }
}

/// A row's menu: query, refresh, copy, remove.
fn row_menu(popup: PopupMenu, entity: &Entity<ClaudhubApp>, entry: &Entry) -> PopupMenu {
    let is_table = matches!(entry, Entry::Table { .. } | Entry::Column { .. });
    let is_connection = matches!(entry, Entry::Connection { .. });
    let (console, tab, refresh, copy, remove) = (
        entity.clone(),
        entity.clone(),
        entity.clone(),
        entity.clone(),
        entity.clone(),
    );
    let (e1, e5, e2, e3, e4) = (
        entry.clone(),
        entry.clone(),
        entry.clone(),
        entry.clone(),
        entry.clone(),
    );
    popup
        .item(
            PopupMenuItem::new(if is_table {
                tr!("db-query-table")
            } else {
                tr!("db-new-console")
            })
            .icon(icon("play"))
            .on_click(move |_, window, cx| {
                console.update(cx, |this, cx| {
                    this.open_db_console(
                        &e1,
                        crate::ui::db_query::ConsoleTarget::Current,
                        window,
                        cx,
                    )
                });
            }),
        )
        // The same thing in a console of its own: the one gesture that makes
        // two tables readable side by side, which is what a click cannot do
        // without leaving a tab behind on every row one passes.
        .item(
            PopupMenuItem::new(tr!("db-query-in-new-tab"))
                .icon(icon("file-code"))
                .on_click(move |_, window, cx| {
                    tab.update(cx, |this, cx| {
                        this.open_db_console(
                            &e5,
                            crate::ui::db_query::ConsoleTarget::NewTab,
                            window,
                            cx,
                        )
                    });
                }),
        )
        .separator()
        .item(
            PopupMenuItem::new(tr!("action-refresh"))
                .icon(icon("refresh-cw"))
                .on_click(move |_, _window, cx| {
                    refresh.update(cx, |this, cx| this.db_refresh_entry(&e2, cx));
                }),
        )
        .item(
            PopupMenuItem::new(tr!("db-copy-name"))
                .icon(icon("copy"))
                .on_click(move |_, _window, cx| {
                    copy.update(cx, |this, cx| this.db_copy_name(&e3, cx));
                }),
        )
        .when(is_connection, |popup| {
            popup.separator().item(
                PopupMenuItem::new(tr!("db-remove-connection"))
                    .icon(icon("trash-2"))
                    .on_click(move |_, _window, cx| {
                        remove.update(cx, |this, cx| this.db_remove_connection(&e4, cx));
                    }),
            )
        })
}

impl ClaudhubApp {
    /// Refreshes what a row names, and nothing more: reopening a whole server to
    /// re-read one table would be one command per database.
    pub(super) fn db_refresh_entry(&mut self, entry: &Entry, cx: &mut Context<Self>) {
        match *entry {
            Entry::Connection { connection } => self.db_refresh(Some(connection), cx),
            Entry::Database {
                connection,
                database,
            } => {
                if let Some(state) = self.database_mut(connection, database) {
                    state.tables = Load::Idle;
                    let expanded = state.expanded;
                    if expanded {
                        self.db_load_tables(connection, database, cx);
                    }
                }
                self.db_rebuild(cx);
            }
            Entry::Table {
                connection,
                database,
                table,
            }
            | Entry::Column {
                connection,
                database,
                table,
                ..
            } => {
                if let Some(state) = self.table_mut(connection, database, table) {
                    state.columns = Load::Idle;
                    let expanded = state.expanded;
                    if expanded {
                        self.db_load_columns(connection, database, table, cx);
                    }
                }
                self.db_rebuild(cx);
            }
            Entry::Status { .. } => {}
        }
    }

    fn db_copy_name(&mut self, entry: &Entry, cx: &mut Context<Self>) {
        let Some(name) = self.db_entry_name(entry) else {
            return;
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(name));
    }

    fn db_entry_name(&self, entry: &Entry) -> Option<String> {
        Some(match *entry {
            Entry::Connection { connection } => self.connection_at(connection)?.config.label(),
            Entry::Database {
                connection,
                database,
            } => self.database_at(connection, database)?.info.name.clone(),
            Entry::Table {
                connection,
                database,
                table,
            } => self
                .table_at(connection, database, table)?
                .info
                .name
                .clone(),
            Entry::Column {
                connection,
                database,
                table,
                column,
            } => self
                .table_at(connection, database, table)?
                .columns
                .ready()?
                .get(column)?
                .name
                .clone(),
            Entry::Status { .. } => return None,
        })
    }

    /// Removes a connection from the settings.
    ///
    /// By its **value** and not by its index: the settings may have been
    /// rewritten since the menu opened, and a stale index would delete the
    /// neighbour.
    fn db_remove_connection(&mut self, entry: &Entry, cx: &mut Context<Self>) {
        let Entry::Connection { connection } = *entry else {
            return;
        };
        let Some(config) = self.connection_at(connection).map(|s| s.config.clone()) else {
            return;
        };
        Settings::update_global(cx, |settings| {
            settings.databases.retain(|other| other != &config);
        });
        self.sync_db_connections(cx);
    }
}
