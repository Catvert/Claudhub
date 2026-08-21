//! MySQL et MariaDB, par `sqlx`.
//!
//! Le schéma se lit dans `information_schema`, qui est une base comme une
//! autre : une requête suffit pour toutes les colonnes d'un schéma, là où
//! SQLite demande un pragma par table.
//!
//! Rien n'est filtré à l'écriture : ce que le compte de connexion a le droit
//! de faire, la console SQL le fait. Un compte en lecture seule est la seule
//! barrière qui tienne — un filtre sur le texte de la requête se contourne en
//! une ligne et interdit ce qui est légitime.

use std::collections::BTreeMap;

use anyhow::{Context as _, Result};
use futures::TryStreamExt as _;
use sqlx::{
    mysql::{MySqlConnectOptions, MySqlConnection, MySqlRow},
    Column as _, ConnectOptions as _, Either, Row as _, ValueRef as _,
};

use super::{bytes_to_string, Cell, Column, Connection, Database, Rows, Table};

/// Les bases du serveur, que personne ne vient explorer.
const SYSTEM_SCHEMAS: [&str; 4] = ["information_schema", "mysql", "performance_schema", "sys"];

/// Ouvre une connexion, éventuellement positionnée sur une base.
///
/// Le délai est posé ici et non seulement autour de la requête : un serveur
/// injoignable tiendrait sinon le worker jusqu'à ce que le noyau abandonne,
/// soit deux minutes.
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
                "la connexion à {}:{} a expiré",
                connection.host(),
                connection.port()
            )
        })?
        .with_context(|| format!("connexion à {}:{}", connection.host(), connection.port()))
}

/// Les points d'interrogation d'une clause `IN (?, ?, …)`.
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
            // MySQL écrit le commentaire « VIEW » sur chacune de ses vues :
            // l'afficher n'apprendrait rien que l'icône ne dise déjà.
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

/// Une colonne, telle que les deux requêtes ci-dessus la rendent : elles ne
/// commencent pas par les mêmes champs, mais les nomment de la même façon.
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
    let mut stream = sqlx::raw_sql(sql).fetch_many(&mut db);
    let mut out = Rows {
        offset,
        ..Default::default()
    };
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
                }
                if skipped < offset {
                    skipped += 1;
                    continue;
                }
                if out.rows.len() >= limit {
                    out.more = true;
                    break;
                }
                out.rows.push(cells(&row));
            }
        }
    }
    Ok(out)
}

/// Écrit le résultat entier au fil de l'eau. Voir `super::export_csv`.
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
    while let Some(item) = stream.try_next().await? {
        if let Either::Right(row) = item {
            if !header {
                out.write_all(
                    super::csv_line(row.columns().iter().map(|column| Some(column.name())))
                        .as_bytes(),
                )?;
                header = true;
            }
            out.write_all(super::csv_line(cells(&row).iter().map(|c| c.as_deref())).as_bytes())?;
            written += 1;
        }
    }
    Ok(written)
}

fn cells(row: &MySqlRow) -> Vec<Cell> {
    (0..row.columns().len())
        .map(|index| value_to_cell(row, index))
        .collect()
}

/// Une valeur de colonne, en texte.
///
/// L'ordre des tentatives est celui du plus précis au plus général, et le
/// texte passe en premier : un `DECIMAL(20,4)` arrive comme une chaîne et le
/// faire passer par un `f64` l'arrondirait.
fn value_to_cell(row: &MySqlRow, index: usize) -> Cell {
    match row.try_get_raw(index) {
        Ok(value) if value.is_null() => return None,
        Ok(_) => {}
        Err(_) => return Some("?".to_string()),
    }
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
    // Le type JSON natif de MySQL ne se décode pas en `String`.
    if let Ok(value) = row.try_get::<serde_json::Value, _>(index) {
        return Some(value.to_string());
    }
    if let Ok(value) = row.try_get::<Vec<u8>, _>(index) {
        return Some(bytes_to_string(value));
    }
    Some("<?>".to_string())
}
