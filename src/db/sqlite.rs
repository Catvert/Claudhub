//! SQLite, through `sqlx`.
//!
//! The file is opened **read-only**: the SQL console is for querying a
//! development database while reviewing the code that writes it, and a
//! `DELETE` from a slipped finger is never a service there. The engine
//! refuses the write itself, which beats a filter of our own on the query
//! text — you cannot tell what a query does by reading it.
//!
//! The schema is read through the pragmas, which can be queried like tables
//! (`pragma_table_info(…)`): that is what makes it possible to join and filter
//! them in SQL instead of parsing an output.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Context as _, Result};
use sqlx::{
    sqlite::{Sqlite, SqliteConnectOptions, SqliteConnection, SqliteRow},
    ConnectOptions as _, Row as _, TypeInfo as _, ValueRef as _,
};

use super::{bytes_to_string, Cell, Column, Connection, Database, Rows, Table};

async fn open(connection: &Connection) -> Result<SqliteConnection> {
    let path = super::expand(&connection.path);
    anyhow::ensure!(path.is_file(), "no database file at {}", path.display());
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .read_only(true)
        // A database a development server is busy writing is locked for a few
        // milliseconds: waiting beats a "database is locked" on every click.
        .busy_timeout(super::CONNECT_TIMEOUT);
    tokio::time::timeout(super::CONNECT_TIMEOUT, options.connect())
        .await
        .with_context(|| format!("opening {} timed out", path.display()))?
        .with_context(|| format!("opening {}", path.display()))
}

pub async fn databases(connection: &Connection) -> Result<Vec<Database>> {
    let mut db = open(connection).await?;
    let rows = sqlx::query("SELECT name FROM pragma_database_list ORDER BY seq")
        .fetch_all(&mut db)
        .await?;
    rows.into_iter()
        .map(|row| {
            Ok(Database {
                name: row.try_get("name")?,
                charset: None,
                collation: None,
            })
        })
        .collect()
}

pub async fn tables(connection: &Connection, database: &str) -> Result<Vec<Table>> {
    let mut db = open(connection).await?;
    read_tables(&mut db, database).await
}

async fn read_tables(db: &mut SqliteConnection, database: &str) -> Result<Vec<Table>> {
    let sql = format!(
        "SELECT name, type FROM {}.sqlite_master \
         WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%' ORDER BY name",
        super::link::quote(super::Engine::Sqlite, database)
    );
    // The schema's name is an identifier, which SQLite cannot bind: it goes in
    // quoted by `link::quote`, which doubles any quote inside it.
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(db).await?;
    rows.into_iter()
        .map(|row| {
            let kind: String = row.try_get("type")?;
            Ok(Table {
                name: row.try_get("name")?,
                view: kind == "view",
                // SQLite keeps neither an engine, nor a row count, nor a size
                // per table: asking for them would cost a full scan per table,
                // on every opening of a database.
                engine: None,
                rows: None,
                bytes: None,
                collation: None,
                comment: None,
            })
        })
        .collect()
}

pub async fn columns(connection: &Connection, database: &str, table: &str) -> Result<Vec<Column>> {
    let mut db = open(connection).await?;
    read_columns(&mut db, database, table).await
}

pub async fn all_columns(
    connection: &Connection,
    database: &str,
) -> Result<BTreeMap<String, Vec<Column>>> {
    let mut db = open(connection).await?;
    let mut out = BTreeMap::new();
    // SQLite's pragmas can only be queried table by table: there is no
    // `information_schema` to sweep in one go. The gain is intact all the same
    // — this is a single connection, and the connection is what costs.
    for table in read_tables(&mut db, database).await? {
        let columns = read_columns(&mut db, database, &table.name).await?;
        out.insert(table.name, columns);
    }
    Ok(out)
}

async fn read_columns(
    db: &mut SqliteConnection,
    database: &str,
    table: &str,
) -> Result<Vec<Column>> {
    let foreign_keys: HashMap<String, String> =
        sqlx::query("SELECT \"from\", \"table\", \"to\" FROM pragma_foreign_key_list(?1, ?2)")
            .bind(table)
            .bind(database)
            .fetch_all(&mut *db)
            .await?
            .into_iter()
            .map(|row| {
                let from: String = row.try_get("from")?;
                let target_table: String = row.try_get("table")?;
                let target_column: Option<String> = row.try_get("to")?;
                Ok((
                    from,
                    match target_column {
                        Some(column) => format!("{target_table}.{column}"),
                        None => target_table,
                    },
                ))
            })
            .collect::<Result<_>>()?;

    // A column is called unique only if a unique index covers it **alone**: a
    // multi-column unique index says nothing about any one of them taken
    // separately.
    let mut by_index: HashMap<String, Vec<String>> = HashMap::new();
    let indexed = sqlx::query(
        "SELECT il.name AS index_name, ii.name AS column_name \
         FROM pragma_index_list(?1, ?2) AS il, pragma_index_info(il.name) AS ii \
         WHERE il.\"unique\" = 1",
    )
    .bind(table)
    .bind(database)
    .fetch_all(&mut *db)
    .await?;
    for row in indexed {
        let index: String = row.try_get("index_name")?;
        let column: String = row.try_get("column_name")?;
        by_index.entry(index).or_default().push(column);
    }
    let unique: HashSet<String> = by_index
        .into_values()
        .filter(|columns| columns.len() == 1)
        .flatten()
        .collect();

    let rows = sqlx::query(
        "SELECT name, type, \"notnull\", dflt_value, pk \
         FROM pragma_table_info(?1, ?2) ORDER BY cid",
    )
    .bind(table)
    .bind(database)
    .fetch_all(&mut *db)
    .await?;
    rows.into_iter()
        .map(|row| {
            let name: String = row.try_get("name")?;
            let not_null: i64 = row.try_get("notnull")?;
            let primary: i64 = row.try_get("pk")?;
            Ok(Column {
                data_type: row.try_get("type")?,
                // A primary key on an `INTEGER PRIMARY KEY` accepts NULL as far
                // as SQLite is concerned — it is the `rowid` alias — but calling
                // it nullable would lie about what can be put there.
                nullable: not_null == 0 && primary == 0,
                default: row.try_get("dflt_value")?,
                primary_key: primary > 0,
                unique: unique.contains(&name),
                auto_increment: false,
                foreign_key: foreign_keys.get(&name).cloned(),
                charset: None,
                collation: None,
                comment: None,
                name,
            })
        })
        .collect()
}

pub async fn query(
    connection: &Connection,
    sql: &str,
    offset: usize,
    limit: usize,
) -> Result<Rows> {
    let mut db = open(connection).await?;
    super::read_page::<Sqlite, _>(&mut db, sql, offset, limit, |_| cells).await
}

/// Writes the whole result as it streams. See `super::export_csv`.
pub async fn export(
    connection: &Connection,
    sql: &str,
    out: &mut dyn std::io::Write,
) -> Result<u64> {
    let mut db = open(connection).await?;
    super::write_csv::<Sqlite, _>(&mut db, sql, out, |_| cells).await
}

fn cells(row: &SqliteRow) -> Vec<Cell> {
    (0..row.columns().len())
        .map(|index| value_to_cell(row, index).map(Into::into))
        .collect()
}

/// A value, as text.
///
/// **SQLite has no type per column but a type per value**, so the decoding is
/// chosen value by value and not once per column as MySQL allows. The name the
/// value carries says which decoding can succeed; the cascade below stays as
/// the fallback for anything else, and it is what keeps the choice invisible.
/// Going through the cascade for every cell meant up to four failed `try_get`
/// per value, each of them allocating an error.
fn value_to_cell(row: &SqliteRow, index: usize) -> Option<String> {
    let Ok(value) = row.try_get_raw(index) else {
        return Some("?".to_string());
    };
    if value.is_null() {
        return None;
    }
    let kind = value.type_info();
    match kind.name() {
        "TEXT" => {
            if let Ok(value) = row.try_get::<String, _>(index) {
                return Some(value);
            }
        }
        "INTEGER" => {
            if let Ok(value) = row.try_get::<i64, _>(index) {
                return Some(value.to_string());
            }
        }
        "REAL" => {
            if let Ok(value) = row.try_get::<f64, _>(index) {
                return Some(value.to_string());
            }
        }
        "BLOB" => {
            if let Ok(value) = row.try_get::<Vec<u8>, _>(index) {
                return Some(bytes_to_string(value));
            }
        }
        _ => {}
    }
    cascade(row, index)
}

/// Every decoding, tried in turn.
///
/// The order matters: the first successful decode decides the display. Text
/// first, because an integer stored as text has to read as it was written.
fn cascade(row: &SqliteRow, index: usize) -> Option<String> {
    if let Ok(value) = row.try_get::<String, _>(index) {
        return Some(value);
    }
    if let Ok(value) = row.try_get::<i64, _>(index) {
        return Some(value.to_string());
    }
    if let Ok(value) = row.try_get::<f64, _>(index) {
        return Some(value.to_string());
    }
    if let Ok(value) = row.try_get::<bool, _>(index) {
        return Some(value.to_string());
    }
    if let Ok(value) = row.try_get::<Vec<u8>, _>(index) {
        return Some(bytes_to_string(value));
    }
    Some("<?>".to_string())
}
