//! The database layer.
//!
//! It is to `src/ui/db*.rs` what `src/git/` is to the review: types and
//! functions that talk to the outside world, without a line of gpui, and that
//! can be tested without a window.
//!
//! **One driver, `sqlx`**, for both engines — and for the third one we would
//! add. It is async end to end, hence `runtime::executor`'s shared executor:
//! the worker handling a command does a `block_on` and waits exactly as it
//! waited for `git`. What that buys and a blocking driver could not give: a
//! timeout that **really cancels** — the future is dropped, and the driver
//! closes the connection mid-flight.
//!
//! **One connection per query, never kept.** A panel holding a connection open
//! on a server nobody queries any more ties up a descriptor and a server-side
//! process for nothing, and discovers the network is down at the worst moment.
//! A `connect` costs a few milliseconds locally, and these commands live in the
//! network queue anyway.
//!
//! **Read-only is SQLite's.** The file is opened with `SQLITE_OPEN_READONLY`:
//! an `UPDATE` run by mistake in the SQL console fails there, and that is what
//! one wants from an explorer. For MySQL, the server decides — the connection
//! account's rights are the only barrier that counts, and adding a second one
//! here would forbid an `UPDATE` the user is entitled to make.

pub mod mysql;
pub mod scope;
pub mod sqlite;

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

/// MySQL's and MariaDB's port when the connection does not name one.
pub const DEFAULT_MYSQL_PORT: u16 = 3306;

/// Past this, the connection is deemed impossible.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Past this, the query is abandoned.
///
/// Really abandoned: `timeout` drops the future, and the driver closes the
/// connection mid-flight. That is what a blocking driver cannot do — it has to
/// be asked to stop by some means of its own, and what it did not plan for does
/// not get interrupted at all.
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(30);

/// Wraps a read in the common timeout.
///
/// At the entry of each public function and not around each query: an
/// introspection makes several in a row, and it is the whole gesture that is
/// abandoned, not its third query.
async fn with_timeout<T>(future: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(QUERY_TIMEOUT, future)
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "the database did not answer in {} s",
                QUERY_TIMEOUT.as_secs()
            )
        })?
}

/// A connection's engine.
///
/// Two, and the third can be read in this type: adding PostgreSQL means one
/// variant here and one module beside `sqlite` and `mysql`, with nothing to
/// change in the protocol or in the views.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    #[default]
    Sqlite,
    /// MySQL and MariaDB, which speak the same protocol and declare their
    /// schema in the same `information_schema`.
    Mysql,
}

impl Engine {
    pub fn as_key(self) -> &'static str {
        match self {
            Self::Sqlite => "sqlite",
            Self::Mysql => "mysql",
        }
    }

    pub fn from_key(key: &str) -> Self {
        match key {
            "mysql" | "mariadb" => Self::Mysql,
            _ => Self::Sqlite,
        }
    }

    /// What the form offers, in order.
    pub const ALL: [Engine; 2] = [Engine::Sqlite, Engine::Mysql];

    pub fn label(self) -> &'static str {
        match self {
            Self::Sqlite => "SQLite",
            Self::Mysql => "MySQL / MariaDB",
        }
    }
}

/// A connection as the settings carry it.
///
/// **A flat structure and not an enum with a payload**, even though the two
/// engines have almost no field in common: that is what makes the settings form
/// possible — one row, fields shown or hidden according to the engine — and
/// what makes a file written by an earlier version still readable,
/// `#[serde(default)]` filling in what is missing. It is
/// `DatabaseConnectionContent`'s choice on the Zed side, and for the same
/// reasons.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Connection {
    /// What the panel shows. Empty, it is derived from the file or from
    /// `user@host`.
    pub name: String,
    pub engine: Engine,
    /// SQLite: the file's path, `~/` expanded at opening.
    pub path: String,
    pub host: String,
    /// `0` means "the engine's": a default port hard-coded in the settings file
    /// would age badly, and zero is not a port.
    pub port: u16,
    pub user: String,
    /// In the clear in the settings file, which is 0600 without being a vault
    /// for all that: prefer a read-only account.
    pub password: String,
    /// The databases to show. Empty: all but the system ones.
    pub databases: Vec<String>,
    /// Which of them belong to the checkout being looked at — patterns with
    /// `*` and `{slug}` / `{worktree}` / `{branch}`, see `db::scope`.
    ///
    /// **Not part of `key()`**: it says what to show, not which connection this
    /// is, and a console reopened from another worktree must stay the same
    /// console.
    pub scope: String,
}

/// The password is never written.
///
/// That is what lets a `Cmd` carry the whole connection rather than have the
/// worker read it back: the protocol is logged — `log::warn!` on a failure,
/// `{cmd:?}` under a debugger — and a secret crossing a derived `Debug` ends up
/// in a trace file.
impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("name", &self.name)
            .field("engine", &self.engine)
            .field("path", &self.path)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("password", &if self.password.is_empty() { "" } else { "…" })
            .field("databases", &self.databases)
            .field("scope", &self.scope)
            .finish()
    }
}

impl Connection {
    /// The displayed name: its own, or the one derived from the address.
    pub fn label(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.trim().to_string();
        }
        match self.engine {
            Engine::Sqlite => std::path::Path::new(&self.path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.path.clone()),
            Engine::Mysql => format!("{}@{}", self.user(), self.host()),
        }
    }

    /// The address, as the panel's row shows it second.
    pub fn detail(&self) -> String {
        match self.engine {
            Engine::Sqlite => self.path.clone(),
            Engine::Mysql => format!("{}:{}", self.host(), self.port()),
        }
    }

    pub fn host(&self) -> String {
        let host = self.host.trim();
        if host.is_empty() {
            "localhost".to_string()
        } else {
            host.to_string()
        }
    }

    pub fn port(&self) -> u16 {
        if self.port == 0 {
            DEFAULT_MYSQL_PORT
        } else {
            self.port
        }
    }

    pub fn user(&self) -> String {
        let user = self.user.trim();
        if user.is_empty() {
            "root".to_string()
        } else {
            user.to_string()
        }
    }

    /// A connection with no address is not a connection: it does not appear in
    /// the panel rather than failing there on every opening.
    pub fn is_usable(&self) -> bool {
        match self.engine {
            Engine::Sqlite => !self.path.trim().is_empty(),
            Engine::Mysql => true,
        }
    }

    /// What is needed to recognise a connection from one render to the next.
    ///
    /// **The password is not in it**: it is a secret, it does not change the
    /// schema, and this key serves to find a connection's state again when the
    /// settings have just been rewritten.
    pub fn key(&self) -> String {
        match self.engine {
            Engine::Sqlite => format!("sqlite:{}", self.path),
            Engine::Mysql => format!(
                "mysql:{}@{}:{}/{}",
                self.user(),
                self.host(),
                self.port(),
                self.databases.join(",")
            ),
        }
    }
}

/// One database of a connection.
///
/// For SQLite it is `main` and whatever `ATTACH` would have added: the level
/// exists all the same, otherwise the tree would have two shapes depending on
/// the engine and everything walking it would have to deal with that.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Database {
    pub name: String,
    pub charset: Option<String>,
    pub collation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Table {
    pub name: String,
    /// A view and not a table. A boolean rather than a two-variant enum: there
    /// is nothing else to distinguish here.
    pub view: bool,
    pub engine: Option<String>,
    /// Approximate on MySQL, which returns the optimiser's estimate, and unknown
    /// on SQLite, which keeps it nowhere.
    pub rows: Option<u64>,
    pub bytes: Option<u64>,
    pub collation: Option<String>,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Column {
    pub name: String,
    /// The type as the engine declares it: `varchar(255)`, and not an
    /// abstraction of ours that would lose the length.
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub primary_key: bool,
    pub unique: bool,
    pub auto_increment: bool,
    /// The `table.column` targeted, when the column is a foreign key.
    pub foreign_key: Option<String>,
    pub charset: Option<String>,
    pub collation: Option<String>,
    pub comment: Option<String>,
}

/// A result value, already turned into text.
///
/// **`None` is `NULL`, and that is not the same as the string "NULL".** A
/// `TEXT` column routinely contains the word, and confusing them is paid for
/// three times: the table shows them alike, the CSV export writes `NULL` where
/// an empty field is expected, and in-memory sorting files them together. Text
/// rather than a typed value is still the right level — the view does not know
/// the engine's types, and making them cross would mean one value enum per
/// driver.
pub type Cell = Option<String>;

/// A query's result, one page at a time.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Rows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Cell>>,
    /// What a write touched, when the query returns no rows.
    pub affected: Option<u64>,
    /// Index of the first row returned within the whole result.
    pub offset: usize,
    /// The result continues beyond this page.
    pub more: bool,
}

impl Rows {
    /// Appends a page after the one being looked at.
    ///
    /// It is what scrolling does when it reaches the bottom: the displayed
    /// window **grows** instead of moving, so the rows just read do not go out
    /// of sight. The columns are the first page's — it is the same query.
    pub fn extend(&mut self, next: Rows) {
        if self.columns.is_empty() {
            self.columns = next.columns;
        }
        self.rows.extend(next.rows);
        self.more = next.more;
    }
}

/// A connection's databases.
pub async fn databases(connection: &Connection) -> Result<Vec<Database>> {
    with_timeout(async {
        match connection.engine {
            Engine::Sqlite => sqlite::databases(connection).await,
            Engine::Mysql => mysql::databases(connection).await,
        }
    })
    .await
}

/// A database's tables and views.
pub async fn tables(connection: &Connection, database: &str) -> Result<Vec<Table>> {
    with_timeout(async {
        match connection.engine {
            Engine::Sqlite => sqlite::tables(connection, database).await,
            Engine::Mysql => mysql::tables(connection, database).await,
        }
    })
    .await
}

/// A table's columns.
pub async fn columns(connection: &Connection, database: &str, table: &str) -> Result<Vec<Column>> {
    with_timeout(async {
        match connection.engine {
            Engine::Sqlite => sqlite::columns(connection, database, table).await,
            Engine::Mysql => mysql::columns(connection, database, table).await,
        }
    })
    .await
}

/// The columns of **every** table of a database, by table.
///
/// One query and not one per table: indexing a three-hundred-table schema for
/// the filter and the completions would otherwise cost three hundred connections.
pub async fn all_columns(
    connection: &Connection,
    database: &str,
) -> Result<BTreeMap<String, Vec<Column>>> {
    with_timeout(async {
        match connection.engine {
            Engine::Sqlite => sqlite::all_columns(connection, database).await,
            Engine::Mysql => mysql::all_columns(connection, database).await,
        }
    })
    .await
}

/// Runs `sql` and returns the page of `limit` rows starting at `offset`.
///
/// **Paging is done by reading, not by rewriting the query.** Adding a `LIMIT`
/// to what the user wrote would mean understanding their query — a `LIMIT`
/// already present, a union, a procedure — and rewriting it, which is the
/// surest way to make them run something other than what they read. The rows
/// preceding the page are therefore really produced by the engine, then thrown
/// away; those that follow are never read.
pub async fn query(
    connection: &Connection,
    database: Option<&str>,
    sql: &str,
    offset: usize,
    limit: usize,
) -> Result<Rows> {
    with_timeout(async {
        match connection.engine {
            Engine::Sqlite => sqlite::query(connection, sql, offset, limit).await,
            Engine::Mysql => mysql::query(connection, database, sql, offset, limit).await,
        }
    })
    .await
}

/// True if `sql`'s result can be sorted by the engine.
///
/// Sorting goes through `order_by`, which **wraps** the query: two things
/// prevent that, and it is better not to offer the gesture than to offer it and
/// fail. A query we do not know how to wrap, first — see `order_by`. Two
/// columns with the same name, second: MySQL refuses a derived table whose two
/// columns are named alike, which is the common case of a join written
/// `SELECT * FROM a JOIN b`.
pub fn can_order(sql: &str, columns: &[String]) -> bool {
    if columns.is_empty() {
        return false;
    }
    let mut seen = std::collections::HashSet::new();
    if !columns.iter().all(|name| seen.insert(name.to_lowercase())) {
        return false;
    }
    order_by(sql, 0, true).is_some()
}

/// The query, wrapped in what is needed to sort it on its `column`-th column.
///
/// **Clicking a header asks the engine, it does not sort the page.** Sorting
/// what is in front of you in memory would lie from the second page on: the
/// thousand rows loaded would be ordered among themselves, and the result's
/// greatest row would stay on the next page. It is the only thing Claudhub adds
/// to the user's query, and it is bounded by everything that follows.
///
/// **The query is not rewritten, it is wrapped**: `SELECT * FROM (…) ORDER BY`.
/// Understanding the query in order to insert an `ORDER BY` — a sort already
/// present, a union, a `LIMIT` — is the surest way to make it run something
/// other than what it reads; a derived table, on the other hand, does not change
/// the meaning of what it contains.
///
/// **We order by the column's rank and not by its name**: a rank does not need
/// quoting, whereas a name would, under rules specific to each engine, and a
/// computed column is called `count(*)`.
///
/// **The parentheses are on their own line**, which puts the `)` out of reach of
/// a `--` comment ending the query.
///
/// `None` when the query does not let itself be wrapped: several statements —
/// the parenthesis would fall between two — or something other than a read. The
/// semicolon is looked for in the raw text, so a query carrying a `;` inside a
/// string literal loses sorting: that is the sense of the refusal, and it costs
/// only one unavailable gesture.
pub fn order_by(sql: &str, column: usize, ascending: bool) -> Option<String> {
    let body = sql.trim().trim_end_matches(';').trim_end();
    if body.is_empty() || body.contains(';') {
        return None;
    }
    let head = body
        .split(|c: char| c.is_whitespace() || c == '(')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    // The empty string is the case of a query opening on a parenthesis,
    // `(SELECT …) UNION (SELECT …)`.
    if !matches!(head.as_str(), "" | "SELECT" | "WITH" | "VALUES" | "TABLE") {
        return None;
    }
    let direction = if ascending { "ASC" } else { "DESC" };
    Some(format!(
        "SELECT * FROM (\n{body}\n) AS claudhub_result ORDER BY {} {direction}",
        column + 1
    ))
}

/// One table row, values escaped and terminated by a newline.
///
/// The escaping is RFC 4180's: only what needs it is quoted — the separator, a
/// quote, a newline — and a quote is doubled. The terminator is a `\n` and not
/// the RFC's `\r\n`: everything that reads CSV accepts both, and a file opened
/// in your editor beside the code has no business being littered with carriage
/// returns.
///
/// **A null value is an empty field**, which is every SQL export's convention —
/// and the reason `Cell` distinguishes `NULL` from the string "NULL", which
/// comes out here in quotes.
pub fn sep_line<'a>(fields: impl IntoIterator<Item = Option<&'a str>>, separator: char) -> String {
    let mut line = String::new();
    for (index, field) in fields.into_iter().enumerate() {
        if index > 0 {
            line.push(separator);
        }
        let Some(value) = field else { continue };
        if value.contains([separator, '"', '\n', '\r']) {
            line.push('"');
            for c in value.chars() {
                if c == '"' {
                    line.push('"');
                }
                line.push(c);
            }
            line.push('"');
        } else {
            line.push_str(value);
        }
    }
    line.push('\n');
    line
}

/// One CSV row: what goes into a **file**.
pub fn csv_line<'a>(fields: impl IntoIterator<Item = Option<&'a str>>) -> String {
    sep_line(fields, ',')
}

/// A line of tab-separated values: what goes to the **clipboard**.
///
/// The two formats differ only by their destination, and that is what settles
/// it: **a clipboard is pasted, a file is opened.** A paste lands in a
/// spreadsheet grid or in a message, where the tab keeps the columns and the
/// comma makes a whole row into a single cell; a file, for its part, is read by
/// a program that knows how to parse CSV. It is the split DataGrip and PhpStorm
/// make.
pub fn tsv_line<'a>(fields: impl IntoIterator<Item = Option<&'a str>>) -> String {
    sep_line(fields, '\t')
}

/// Past this, the export is abandoned.
///
/// Ten times a query's timeout: an export covers the **whole** result where the
/// console only reads one page, and a table of a million rows takes more than
/// thirty seconds to come out with nothing wrong.
pub const EXPORT_TIMEOUT: Duration = Duration::from_secs(600);

/// Writes `sql`'s whole result into a CSV file, and returns its row count.
///
/// **The query is replayed, and the result is never held in memory.** Exporting
/// what is displayed would export only one page — never what one wants from an
/// export — and loading everything to write it afterwards would hold a million
/// rows on the heap only to copy them straight out. The rows therefore go from
/// the engine to the file one by one.
///
/// The write is **blocking in the middle of an async task**, which is accepted:
/// it is a local file, the executor carries nothing but database work, and a
/// worker that writes waits exactly as it waits on a socket.
pub async fn export_csv(
    connection: &Connection,
    database: Option<&str>,
    sql: &str,
    path: &std::path::Path,
) -> Result<u64> {
    let file =
        std::fs::File::create(path).with_context(|| format!("cannot write {}", path.display()))?;
    let mut out = std::io::BufWriter::new(file);
    let written = tokio::time::timeout(EXPORT_TIMEOUT, async {
        match connection.engine {
            Engine::Sqlite => sqlite::export(connection, sql, &mut out).await,
            Engine::Mysql => mysql::export(connection, database, sql, &mut out).await,
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("l'export n'a pas abouti en {} s", EXPORT_TIMEOUT.as_secs()))??;
    std::io::Write::flush(&mut out)?;
    Ok(written)
}

/// The bytes of a binary value, as text.
///
/// MySQL files its JSON type in a `LONGTEXT` with a binary collation, and binary
/// columns often carry readable text: showing the text when the bytes are valid
/// UTF-8 beats hiding it behind a count.
pub(crate) fn bytes_to_string(bytes: Vec<u8>) -> String {
    match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => format!("<{}>", size(error.as_bytes().len() as u64)),
    }
}

/// A size in bytes, in the unit that suits it. Language-neutral: these strings
/// appear in the middle of values, not in a translated label.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024. && unit + 1 < UNITS.len() {
        value /= 1024.;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// A row count, abbreviated: a table of six million rows must not take up the
/// whole width of a narrow panel.
pub fn count(rows: u64) -> String {
    if rows >= 1_000_000 {
        format!("{:.1}M", rows as f64 / 1_000_000.)
    } else if rows >= 10_000 {
        format!("{:.1}k", rows as f64 / 1_000.)
    } else {
        rows.to_string()
    }
}

/// A SQLite file's path, `~/` expanded.
///
/// A path typed into a form is written `~/dev/base.sqlite` — that is how it is
/// given to a shell — and passing it as it is to `std::fs` would look for a
/// folder named `~` in the current directory.
pub(crate) fn expand(path: &str) -> std::path::PathBuf {
    match path.trim().strip_prefix("~/") {
        Some(rest) => match directories::UserDirs::new() {
            Some(dirs) => dirs.home_dir().join(rest),
            None => std::path::PathBuf::from(path.trim()),
        },
        None => std::path::PathBuf::from(path.trim()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::executor::block_on;
    use sqlx::ConnectOptions as _;

    /// A result's values, with null made visible.
    fn shown(rows: &Rows) -> Vec<Vec<&str>> {
        rows.rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|cell| cell.as_deref().unwrap_or("<null>"))
                    .collect()
            })
            .collect()
    }

    fn sqlite_at(path: &std::path::Path) -> Connection {
        Connection {
            name: String::new(),
            engine: Engine::Sqlite,
            path: path.to_string_lossy().into_owned(),
            ..Default::default()
        }
    }

    /// The full round on a real database: the tree's three levels, what a column
    /// declares, and a query's paging.
    ///
    /// The test goes through `block_on`, so through the shared executor: it is
    /// the same bridge as the workers', and it is the one we want to exercise.
    #[test]
    fn sqlite_introspection_and_paging() {
        let path = std::env::temp_dir().join(format!("claudhub-db-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let connection = sqlite_at(&path);

        block_on(async {
            // The database is created by a **writable** connection: the one this
            // module opens is read-only, and that is precisely what the last
            // case checks.
            let mut writable = sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true)
                .connect()
                .await
                .unwrap();
            sqlx::raw_sql(
                "CREATE TABLE users (
                     id INTEGER PRIMARY KEY,
                     email TEXT NOT NULL UNIQUE,
                     name TEXT DEFAULT 'anon');
                 CREATE TABLE posts (
                     id INTEGER PRIMARY KEY,
                     user_id INTEGER NOT NULL REFERENCES users(id));
                 CREATE VIEW v_users AS SELECT * FROM users;
                 INSERT INTO users (email, name)
                     VALUES ('a@example.com', 'Ada'), ('b@example.com', NULL);",
            )
            .execute(&mut writable)
            .await
            .unwrap();
            drop(writable);

            let dbs = databases(&connection).await.unwrap();
            assert_eq!(
                dbs.iter().map(|db| db.name.as_str()).collect::<Vec<_>>(),
                ["main"]
            );

            let listed = tables(&connection, "main").await.unwrap();
            assert_eq!(
                listed
                    .iter()
                    .map(|t| (t.name.as_str(), t.view))
                    .collect::<Vec<_>>(),
                [("posts", false), ("users", false), ("v_users", true)]
            );

            let users = columns(&connection, "main", "users").await.unwrap();
            assert_eq!(users.len(), 3);
            assert!(users[0].primary_key && users[0].name == "id");
            assert!(!users[1].nullable && users[1].unique);
            assert_eq!(users[2].default.as_deref(), Some("'anon'"));

            let posts = columns(&connection, "main", "posts").await.unwrap();
            assert_eq!(posts[1].foreign_key.as_deref(), Some("users.id"));

            let all = all_columns(&connection, "main").await.unwrap();
            assert_eq!(all.len(), 3);
            assert_eq!(all["posts"][1].foreign_key.as_deref(), Some("users.id"));

            let page = query(
                &connection,
                None,
                "SELECT id, email, name FROM users ORDER BY id",
                0,
                1000,
            )
            .await
            .unwrap();
            assert_eq!(page.columns, ["id", "email", "name"]);
            // The second row has a null `name`, and it is **`None`** and not the
            // string "NULL": the whole export and the whole sort depend on it.
            assert_eq!(
                shown(&page),
                [
                    ["1", "a@example.com", "Ada"],
                    ["2", "b@example.com", "<null>"]
                ]
            );
            assert!(!page.more);

            let first = query(&connection, None, "SELECT id FROM users ORDER BY id", 0, 1)
                .await
                .unwrap();
            assert_eq!(shown(&first), [["1"]]);
            assert!(first.more, "a full page announces the next one");

            let second = query(&connection, None, "SELECT id FROM users ORDER BY id", 1, 1)
                .await
                .unwrap();
            assert_eq!(shown(&second), [["2"]]);
            assert_eq!(second.offset, 1);
            assert!(!second.more);

            // The file is opened read-only: a write must fail, and it is the
            // engine that says so.
            assert!(query(
                &connection,
                None,
                "INSERT INTO users (email) VALUES ('c@example.com')",
                0,
                10
            )
            .await
            .is_err());
        });

        let _ = std::fs::remove_file(&path);
    }

    /// Sorting and exporting, on the same database: both replay the query, and
    /// it is the only place where one can check that what is added to it is SQL
    /// the engine accepts.
    #[test]
    fn sorting_and_exporting_replay_the_query() {
        let path = std::env::temp_dir().join(format!("claudhub-csv-{}.sqlite", std::process::id()));
        let csv = path.with_extension("csv");
        let _ = std::fs::remove_file(&path);
        let connection = sqlite_at(&path);

        block_on(async {
            let mut writable = sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true)
                .connect()
                .await
                .unwrap();
            sqlx::raw_sql(
                "CREATE TABLE t (id INTEGER PRIMARY KEY, label TEXT);
                 INSERT INTO t (label) VALUES ('a, comma'), (NULL), ('qu\"ote');",
            )
            .execute(&mut writable)
            .await
            .unwrap();
            drop(writable);

            // Sorting is asked of the engine around the query, including when it
            // carries its own semicolon.
            let sorted = order_by("SELECT id, label FROM t;", 0, false).unwrap();
            let page = query(&connection, None, &sorted, 0, 10).await.unwrap();
            assert_eq!(
                shown(&page).iter().map(|row| row[0]).collect::<Vec<_>>(),
                ["3", "2", "1"]
            );

            let written = export_csv(&connection, None, "SELECT id, label FROM t", &csv)
                .await
                .unwrap();
            assert_eq!(written, 3);
        });

        // The header, the null rendered as an empty field, and both escapes.
        assert_eq!(
            std::fs::read_to_string(&csv).unwrap(),
            "id,label\n1,\"a, comma\"\n2,\n3,\"qu\"\"ote\"\n"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&csv);
    }

    /// The query is not rewritten: it is put into a derived table, and the
    /// column's rank saves having to quote its name.
    #[test]
    fn a_query_is_wrapped_to_be_sorted() {
        let wrapped = order_by("SELECT a, b FROM t  ;\n", 1, true).unwrap();
        assert_eq!(
            wrapped,
            "SELECT * FROM (\nSELECT a, b FROM t\n) AS claudhub_result ORDER BY 2 ASC"
        );
        assert!(order_by("select 1", 0, false).unwrap().ends_with("1 DESC"));
        // An opening parenthesis at the head means a parenthesised union.
        assert!(order_by("(SELECT 1) UNION (SELECT 2)", 0, true).is_some());
        // The closing parenthesis is on its own line, out of reach of a comment
        // ending the query.
        let commented = order_by("SELECT a FROM t -- all of it is here", 0, true).unwrap();
        assert!(commented.contains("\n)"), "{commented}");
    }

    /// What cannot be wrapped is not sorted — rather than sorted wrong.
    #[test]
    fn what_cannot_be_wrapped_is_not_sorted() {
        assert!(order_by("", 0, true).is_none());
        assert!(order_by("UPDATE t SET a = 1", 0, true).is_none());
        // Two statements: the parenthesis would fall between them.
        assert!(order_by("SELECT 1; SELECT 2", 0, true).is_none());

        let columns = ["id".to_string(), "name".to_string()];
        assert!(can_order("SELECT id, name FROM t", &columns));
        assert!(!can_order("SELECT id, name FROM t", &[]));
        // MySQL refuses a derived table with two columns of the same name, which
        // a join written `SELECT *` produces all the time.
        let doubled = ["id".to_string(), "ID".to_string()];
        assert!(!can_order("SELECT * FROM a JOIN b", &doubled));
    }

    /// A CSV reads back: the escaping is what guarantees it, and a null is an
    /// empty field there and not the word "NULL".
    #[test]
    fn csv_quotes_only_what_needs_it() {
        assert_eq!(csv_line([Some("a"), Some("b")]), "a,b\n");
        assert_eq!(csv_line([None, Some("")]), ",\n");
        assert_eq!(csv_line([Some("a,b")]), "\"a,b\"\n");
        assert_eq!(csv_line([Some("says \"yes\"")]), "\"says \"\"yes\"\"\"\n");
        assert_eq!(csv_line([Some("two\nlines")]), "\"two\nlines\"\n");
        // The string "NULL" is not a null, and it must not become one on its way
        // through the CSV.
        assert_eq!(csv_line([Some("NULL")]), "NULL\n");
        assert_eq!(csv_line([None]), "\n");
        // The clipboard takes tabs, and therefore does not quote a value
        // carrying a comma — but does quote the one carrying a tab.
        assert_eq!(tsv_line([Some("a,b"), Some("c")]), "a,b\tc\n");
        assert_eq!(tsv_line([Some("a\tb")]), "\"a\tb\"\n");
    }

    /// Growing the window keeps its columns and picks up where it left off.
    #[test]
    fn a_window_grows_by_its_end() {
        let mut first = Rows {
            columns: vec!["id".into()],
            rows: vec![vec![Some("1".into())]],
            more: true,
            ..Default::default()
        };
        first.extend(Rows {
            columns: vec!["id".into()],
            rows: vec![vec![Some("2".into())]],
            offset: 1,
            more: false,
            ..Default::default()
        });
        assert_eq!(shown(&first), [["1"], ["2"]]);
        assert!(!first.more, "the continuation says whether any is left");
    }

    /// A database that does not exist is an error, not a wait: the message is
    /// what the tree's row will show.
    #[test]
    fn a_missing_file_says_so() {
        let connection = sqlite_at(std::path::Path::new("/tmp/claudhub-no-database.sqlite"));
        let error = block_on(databases(&connection)).unwrap_err();
        assert!(error.to_string().contains("no database file"), "{error}");
    }

    #[test]
    fn a_connection_without_a_name_takes_the_one_of_its_address() {
        let file = Connection {
            engine: Engine::Sqlite,
            path: "/srv/app/database.sqlite".into(),
            ..Default::default()
        };
        assert_eq!(file.label(), "database.sqlite");

        let server = Connection {
            engine: Engine::Mysql,
            host: "db.example.com".into(),
            user: "app".into(),
            ..Default::default()
        };
        assert_eq!(server.label(), "app@db.example.com");
        assert_eq!(server.detail(), "db.example.com:3306");
    }

    /// The password is not part of a connection's identity: correcting it in the
    /// settings must not close the tree that had been unfolded.
    #[test]
    fn the_key_ignores_the_password() {
        let mut connection = Connection {
            engine: Engine::Mysql,
            host: "localhost".into(),
            user: "root".into(),
            password: "hunter2".into(),
            ..Default::default()
        };
        let before = connection.key();
        connection.password = "autre".into();
        assert_eq!(before, connection.key());
    }

    /// The password must appear nowhere in a trace: that is what allows the whole
    /// connection to travel inside a `Cmd`.
    #[test]
    fn the_debug_output_hides_the_password() {
        let connection = Connection {
            engine: Engine::Mysql,
            password: "hunter2".into(),
            ..Default::default()
        };
        let shown = format!("{connection:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
    }

    #[test]
    fn volumes_are_abbreviated() {
        assert_eq!(size(512), "512 B");
        assert_eq!(size(2048), "2.0 KB");
        assert_eq!(count(42), "42");
        assert_eq!(count(2_500_000), "2.5M");
    }
}
