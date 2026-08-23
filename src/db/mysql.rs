//! MySQL and MariaDB, through `sqlx`.
//!
//! The schema is read from `information_schema`, which is a database like any
//! other: one query is enough for every column of a schema, where SQLite needs
//! one pragma per table.
//!
//! Nothing is filtered on write: what the connection account is allowed to do,
//! the SQL console does. A read-only account is the only barrier that holds —
//! a filter on the query text is worked around in one line and forbids what is
//! legitimate.

use std::collections::BTreeMap;

use anyhow::{Context as _, Result};
use futures::TryStreamExt as _;
use sqlx::{
    mysql::{MySqlConnectOptions, MySqlConnection, MySqlRow},
    Column as _, ConnectOptions as _, Either, Row as _, TypeInfo as _, ValueRef as _,
};

use super::{bytes_to_string, Cell, Column, Connection, Database, Rows, Table};

/// The server's own databases, which nobody comes to explore.
const SYSTEM_SCHEMAS: [&str; 4] = ["information_schema", "mysql", "performance_schema", "sys"];

/// Opens a connection, optionally positioned on a database.
///
/// The timeout is set here and not only around the query: an unreachable
/// server would otherwise hold the worker until the kernel gives up, which is
/// two minutes.
async fn open(connection: &Connection, database: Option<&str>) -> Result<MySqlConnection> {
    let mut options = MySqlConnectOptions::new()
        .host(&connection.host())
        .port(connection.port())
        .username(&connection.user());
    if !connection.password.is_empty() {
        options = options.password(&connection.password);
    }
    if let Some(database) = database {
        options = options.database(database);
    }
    tokio::time::timeout(super::CONNECT_TIMEOUT, options.connect())
        .await
        .with_context(|| {
            format!(
                "the connection to {}:{} timed out",
                connection.host(),
                connection.port()
            )
        })?
        .with_context(|| format!("connecting to {}:{}", connection.host(), connection.port()))
}

/// The question marks of an `IN (?, ?, …)` clause.
fn placeholders(count: usize) -> String {
    vec!["?"; count].join(", ")
}

pub async fn databases(connection: &Connection) -> Result<Vec<Database>> {
    let mut db = open(connection, None).await?;
    let wanted: Vec<String> = connection
        .databases
        .iter()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect();
    let (clause, values): (String, Vec<String>) = if wanted.is_empty() {
        (
            format!(
                "schema_name NOT IN ({})",
                placeholders(SYSTEM_SCHEMAS.len())
            ),
            SYSTEM_SCHEMAS.iter().map(|name| name.to_string()).collect(),
        )
    } else {
        (
            format!("schema_name IN ({})", placeholders(wanted.len())),
            wanted,
        )
    };
    let sql = format!(
        "SELECT schema_name AS name, \
                default_character_set_name AS charset, \
                default_collation_name AS collation \
         FROM information_schema.schemata WHERE {clause} ORDER BY schema_name"
    );
    let mut request = sqlx::query(&sql);
    for value in &values {
        request = request.bind(value);
    }
    request
        .fetch_all(&mut db)
        .await?
        .into_iter()
        .map(|row| {
            Ok(Database {
                name: row.try_get("name")?,
                charset: row.try_get("charset")?,
                collation: row.try_get("collation")?,
            })
        })
        .collect()
}

pub async fn tables(connection: &Connection, database: &str) -> Result<Vec<Table>> {
    let mut db = open(connection, None).await?;
    sqlx::query(
        "SELECT table_name AS name, table_type, engine, table_rows, \
                CAST(data_length + index_length AS UNSIGNED) AS total_size, \
                table_collation AS collation, table_comment AS comment \
         FROM information_schema.tables WHERE table_schema = ? ORDER BY table_name",
    )
    .bind(database)
    .fetch_all(&mut db)
    .await?
    .into_iter()
    .map(|row| {
        let kind: String = row.try_get("table_type")?;
        let comment: Option<String> = row.try_get("comment")?;
        Ok(Table {
            name: row.try_get("name")?,
            view: kind.contains("VIEW"),
            engine: row.try_get("engine")?,
            rows: row.try_get("table_rows")?,
            bytes: row.try_get("total_size")?,
            collation: row.try_get("collation")?,
            // MySQL writes the comment "VIEW" on every one of its views:
            // showing it would teach nothing the icon does not already say.
            comment: comment.filter(|comment| !comment.is_empty() && comment != "VIEW"),
        })
    })
    .collect()
}

pub async fn columns(connection: &Connection, database: &str, table: &str) -> Result<Vec<Column>> {
    let mut db = open(connection, None).await?;
    let foreign: BTreeMap<String, String> = sqlx::query(
        "SELECT column_name, referenced_table_name, referenced_column_name \
         FROM information_schema.key_column_usage \
         WHERE table_schema = ? AND table_name = ? AND referenced_table_name IS NOT NULL",
    )
    .bind(database)
    .bind(table)
    .fetch_all(&mut db)
    .await?
    .into_iter()
    .map(|row| {
        let column: String = row.try_get("column_name")?;
        let target_table: String = row.try_get("referenced_table_name")?;
        let target_column: String = row.try_get("referenced_column_name")?;
        Ok((column, format!("{target_table}.{target_column}")))
    })
    .collect::<Result<_>>()?;

    sqlx::query(
        "SELECT column_name AS name, column_type, is_nullable, column_default, column_key, \
                extra, character_set_name AS charset, collation_name AS collation, \
                column_comment AS comment \
         FROM information_schema.columns WHERE table_schema = ? AND table_name = ? \
         ORDER BY ordinal_position",
    )
    .bind(database)
    .bind(table)
    .fetch_all(&mut db)
    .await?
    .into_iter()
    .map(|row| {
        let name: String = row.try_get("name")?;
        let target = foreign.get(&name).cloned();
        column(&row, name, target)
    })
    .collect()
}

pub async fn all_columns(
    connection: &Connection,
    database: &str,
) -> Result<BTreeMap<String, Vec<Column>>> {
    let mut db = open(connection, None).await?;
    let foreign: BTreeMap<(String, String), String> = sqlx::query(
        "SELECT table_name, column_name, referenced_table_name, referenced_column_name \
         FROM information_schema.key_column_usage \
         WHERE table_schema = ? AND referenced_table_name IS NOT NULL",
    )
    .bind(database)
    .fetch_all(&mut db)
    .await?
    .into_iter()
    .map(|row| {
        let table: String = row.try_get("table_name")?;
        let column: String = row.try_get("column_name")?;
        let target_table: String = row.try_get("referenced_table_name")?;
        let target_column: String = row.try_get("referenced_column_name")?;
        Ok(((table, column), format!("{target_table}.{target_column}")))
    })
    .collect::<Result<_>>()?;

    let rows = sqlx::query(
        "SELECT table_name, column_name AS name, column_type, is_nullable, column_default, \
                column_key, extra, character_set_name AS charset, collation_name AS collation, \
                column_comment AS comment \
         FROM information_schema.columns WHERE table_schema = ? \
         ORDER BY table_name, ordinal_position",
    )
    .bind(database)
    .fetch_all(&mut db)
    .await?;
    let mut out: BTreeMap<String, Vec<Column>> = BTreeMap::new();
    for row in rows {
        let table: String = row.try_get("table_name")?;
        let name: String = row.try_get("name")?;
        let target = foreign.get(&(table.clone(), name.clone())).cloned();
        out.entry(table)
            .or_default()
            .push(column(&row, name, target)?);
    }
    Ok(out)
}

/// A column, as both queries above return it: they do not start with the same
/// fields, but they name them the same way.
fn column(row: &MySqlRow, name: String, foreign_key: Option<String>) -> Result<Column> {
    let key: String = row.try_get("column_key")?;
    let extra: String = row.try_get("extra")?;
    let nullable: String = row.try_get("is_nullable")?;
    let comment: Option<String> = row.try_get("comment")?;
    Ok(Column {
        data_type: row.try_get("column_type")?,
        nullable: nullable == "YES",
        default: row.try_get("column_default")?,
        primary_key: key == "PRI",
        unique: key == "UNI",
        auto_increment: extra.contains("auto_increment"),
        foreign_key,
        charset: row.try_get("charset")?,
        collation: row.try_get("collation")?,
        comment: comment.filter(|comment| !comment.is_empty()),
        name,
    })
}

pub async fn query(
    connection: &Connection,
    database: Option<&str>,
    sql: &str,
    offset: usize,
    limit: usize,
) -> Result<Rows> {
    let mut db = open(connection, database).await?;
    // The page is asked of the engine when the query lets itself be wrapped —
    // see `super::paged`. The wrap can still be refused at run time: MySQL
    // rejects a derived table whose two columns are named alike, which is the
    // `SELECT * FROM a JOIN b` of every schema. Reading from the start is
    // therefore kept as the fallback.
    if let Some(paged) = super::paged(sql, offset, limit) {
        match run(&mut db, &paged, offset, 0, limit).await {
            Ok(rows) => return Ok(rows),
            Err(error) => {
                log::debug!("the paged query was refused, reading from the start: {error}")
            }
        }
    }
    run(&mut db, sql, offset, offset, limit).await
}

/// Runs `sql` and keeps `limit` rows, `skip` of them dropped on the way.
///
/// `offset` is what the page says of itself; `skip` is how many rows this has
/// to throw away to get there — zero when the engine was asked for the page.
async fn run(
    db: &mut MySqlConnection,
    sql: &str,
    offset: usize,
    skip: usize,
    limit: usize,
) -> Result<Rows> {
    let mut stream = sqlx::raw_sql(sql).fetch_many(db);
    let mut out = Rows {
        offset,
        ..Default::default()
    };
    let mut decoders: Vec<Decoder> = Vec::new();
    let mut skipped = 0;
    while let Some(item) = stream.try_next().await? {
        match item {
            Either::Left(done) => *out.affected.get_or_insert(0) += done.rows_affected(),
            Either::Right(row) => {
                if out.columns.is_empty() {
                    out.columns = row
                        .columns()
                        .iter()
                        .map(|column| column.name().to_string())
                        .collect();
                    decoders = self::decoders(&row);
                }
                if skipped < skip {
                    skipped += 1;
                    continue;
                }
                if out.rows.len() >= limit {
                    out.more = true;
                    break;
                }
                out.rows.push(cells(&row, &decoders));
            }
        }
    }
    Ok(out)
}

/// Writes the whole result as it streams. See `super::export_csv`.
pub async fn export(
    connection: &Connection,
    database: Option<&str>,
    sql: &str,
    out: &mut dyn std::io::Write,
) -> Result<u64> {
    let mut db = open(connection, database).await?;
    let mut stream = sqlx::raw_sql(sql).fetch_many(&mut db);
    let mut written = 0;
    let mut header = false;
    let mut decoders: Vec<Decoder> = Vec::new();
    while let Some(item) = stream.try_next().await? {
        if let Either::Right(row) = item {
            if !header {
                out.write_all(
                    super::csv_line(row.columns().iter().map(|column| Some(column.name())))
                        .as_bytes(),
                )?;
                header = true;
                decoders = self::decoders(&row);
            }
            out.write_all(
                super::csv_line(cells(&row, &decoders).iter().map(|c| c.as_deref())).as_bytes(),
            )?;
            written += 1;
        }
    }
    Ok(written)
}

/// How a column's values are read, decided once per column from the type name
/// the server sends with it.
///
/// The cascade stays, as the fallback: a name this does not know, and a value
/// the chosen decoding refuses, still go through every attempt. What this
/// removes is the failed ones — up to eleven `try_get` per cell, each of them
/// allocating an error, on a page of a thousand rows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Decoder {
    Text,
    Decimal,
    /// Integers, `YEAR` and `BIT` included: signed first, then unsigned, which
    /// is the order the cascade follows.
    Int,
    Real,
    DateTime,
    Date,
    Time,
    Json,
    Bytes,
    /// Nothing known about this name: straight to the cascade.
    Any,
}

/// The decoding a MySQL type name calls for.
///
/// Each name maps to the first attempt of the cascade that can succeed for it,
/// which is what makes the shortcut invisible: `BOOLEAN` is a `TINYINT(1)` and
/// reads as a number, as it always did.
fn decoder_for(name: &str) -> Decoder {
    match name {
        "VARCHAR" | "CHAR" | "TEXT" | "TINYTEXT" | "MEDIUMTEXT" | "LONGTEXT" | "ENUM" => {
            Decoder::Text
        }
        "DECIMAL" => Decoder::Decimal,
        "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "BIGINT" | "BOOLEAN" | "YEAR" | "BIT"
        | "TINYINT UNSIGNED" | "SMALLINT UNSIGNED" | "MEDIUMINT UNSIGNED" | "INT UNSIGNED"
        | "BIGINT UNSIGNED" => Decoder::Int,
        "FLOAT" | "DOUBLE" => Decoder::Real,
        "DATETIME" | "TIMESTAMP" => Decoder::DateTime,
        "DATE" => Decoder::Date,
        "TIME" => Decoder::Time,
        "JSON" => Decoder::Json,
        "BLOB" | "TINYBLOB" | "MEDIUMBLOB" | "LONGBLOB" | "BINARY" | "VARBINARY" => Decoder::Bytes,
        _ => Decoder::Any,
    }
}

/// One decoder per column of a result, read from the row's column definitions.
fn decoders(row: &MySqlRow) -> Vec<Decoder> {
    row.columns()
        .iter()
        .map(|column| decoder_for(column.type_info().name()))
        .collect()
}

fn cells(row: &MySqlRow, decoders: &[Decoder]) -> Vec<Cell> {
    (0..row.columns().len())
        .map(|index| {
            value_to_cell(
                row,
                index,
                decoders.get(index).copied().unwrap_or(Decoder::Any),
            )
        })
        .collect()
}

/// A column value, as text.
fn value_to_cell(row: &MySqlRow, index: usize, decoder: Decoder) -> Cell {
    match row.try_get_raw(index) {
        Ok(value) if value.is_null() => return None,
        Ok(_) => {}
        Err(_) => return Some("?".to_string()),
    }
    match decoder {
        Decoder::Text => {
            if let Ok(value) = row.try_get::<String, _>(index) {
                return Some(value);
            }
        }
        // `DECIMAL` travels as text in both protocols, but sqlx declares no
        // `String` decoding for it — the numeric types live behind the
        // `bigdecimal` and `rust_decimal` features. The checked path therefore
        // refuses it, and every column of a price table read `<?>`. Its raw
        // bytes are the digits themselves, so the unchecked decoding is exact,
        // and it is what keeps a `DECIMAL(20,4)` from going through an `f64`
        // and being rounded.
        Decoder::Decimal => {
            if let Ok(value) = row.try_get_unchecked::<String, _>(index) {
                return Some(value);
            }
        }
        Decoder::Int => {
            if let Ok(value) = row.try_get::<i64, _>(index) {
                return Some(value.to_string());
            }
            if let Ok(value) = row.try_get::<u64, _>(index) {
                return Some(value.to_string());
            }
        }
        Decoder::Real => {
            if let Ok(value) = row.try_get::<f64, _>(index) {
                return Some(value.to_string());
            }
        }
        Decoder::DateTime => {
            if let Ok(value) = row.try_get::<chrono::NaiveDateTime, _>(index) {
                return Some(value.to_string());
            }
        }
        Decoder::Date => {
            if let Ok(value) = row.try_get::<chrono::NaiveDate, _>(index) {
                return Some(value.to_string());
            }
        }
        Decoder::Time => {
            if let Ok(value) = row.try_get::<chrono::NaiveTime, _>(index) {
                return Some(value.to_string());
            }
        }
        Decoder::Json => {
            if let Ok(value) = row.try_get::<serde_json::Value, _>(index) {
                return Some(value.to_string());
            }
        }
        Decoder::Bytes => {
            if let Ok(value) = row.try_get::<Vec<u8>, _>(index) {
                return Some(bytes_to_string(value));
            }
        }
        Decoder::Any => {}
    }
    cascade(row, index)
}

/// Every decoding, tried in turn.
///
/// The order of attempts goes from the most precise to the most general, and
/// text comes first: a `DECIMAL(20,4)` arrives as a string, and putting it
/// through an `f64` would round it.
fn cascade(row: &MySqlRow, index: usize) -> Cell {
    if let Ok(value) = row.try_get::<String, _>(index) {
        return Some(value);
    }
    if let Ok(value) = row.try_get::<i64, _>(index) {
        return Some(value.to_string());
    }
    if let Ok(value) = row.try_get::<u64, _>(index) {
        return Some(value.to_string());
    }
    if let Ok(value) = row.try_get::<f64, _>(index) {
        return Some(value.to_string());
    }
    if let Ok(value) = row.try_get::<bool, _>(index) {
        return Some(value.to_string());
    }
    if let Ok(value) = row.try_get::<chrono::NaiveDateTime, _>(index) {
        return Some(value.to_string());
    }
    if let Ok(value) = row.try_get::<chrono::DateTime<chrono::Utc>, _>(index) {
        return Some(value.to_string());
    }
    if let Ok(value) = row.try_get::<chrono::NaiveDate, _>(index) {
        return Some(value.to_string());
    }
    if let Ok(value) = row.try_get::<chrono::NaiveTime, _>(index) {
        return Some(value.to_string());
    }
    // MySQL's native JSON type does not decode into a `String`.
    if let Ok(value) = row.try_get::<serde_json::Value, _>(index) {
        return Some(value.to_string());
    }
    if let Ok(value) = row.try_get::<Vec<u8>, _>(index) {
        return Some(bytes_to_string(value));
    }
    Some("<?>".to_string())
}
