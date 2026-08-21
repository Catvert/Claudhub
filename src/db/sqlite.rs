//! SQLite, par `sqlx`.
//!
//! Le fichier est ouvert en **lecture seule** : la console SQL sert à
//! interroger une base de développement pendant qu'on relit le code qui
//! l'écrit, et un `DELETE` parti d'un doigt qui a glissé n'y est jamais un
//! service. Le moteur refuse alors l'écriture lui-même, ce qui vaut mieux
//! qu'un filtre à nous sur le texte de la requête — on ne devine pas ce qu'une
//! requête fait en la lisant.
//!
//! Le schéma se lit par les pragmas, qui s'interrogent comme des tables
//! (`pragma_table_info(…)`) : c'est ce qui permet de les joindre et de les
//! filtrer en SQL au lieu d'analyser une sortie.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Context as _, Result};
use futures::TryStreamExt as _;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteConnection, SqliteRow},
    Column as _, ConnectOptions as _, Either, Row as _, ValueRef as _,
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

/// A quoted identifier, for the places where SQL takes no parameter — a schema
/// name in `schema.table`.
fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
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
        quote(database)
    );
    let rows = sqlx::query(&sql).fetch_all(db).await?;
    rows.into_iter()
        .map(|row| {
            let kind: String = row.try_get("type")?;
            Ok(Table {
                name: row.try_get("name")?,
                view: kind == "view",
                // SQLite ne tient ni moteur, ni compte de lignes, ni taille par
                // table : les demander coûterait un balayage complet par table,
                // à chaque ouverture d'une base.
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
    // Les pragmas de SQLite ne s'interrogent que table par table : il n'y a
    // pas d'`information_schema` à balayer d'un coup. Le gain reste entier —
    // c'est une seule connexion, et c'est elle qui coûte.
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

    // Une colonne n'est dite unique que si un index unique porte sur elle
    // **seule** : un index unique à plusieurs colonnes ne dit rien de chacune
    // d'elles prise à part.
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
                // Une clé primaire d'un `INTEGER PRIMARY KEY` accepte NULL pour
                // SQLite — c'est l'alias de `rowid` — mais l'annoncer nullable
                // serait mentir sur ce qu'on peut y mettre.
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
    // `raw_sql` accepte plusieurs instructions — c'est ce qu'on colle depuis un
    // fichier de migration — et `fetch_many` rend au fil de l'eau ce que
    // chacune produit : un compte de lignes touchées pour une écriture, des
    // lignes pour une lecture.
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
    sql: &str,
    out: &mut dyn std::io::Write,
) -> Result<u64> {
    let mut db = open(connection).await?;
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

fn cells(row: &SqliteRow) -> Vec<Cell> {
    (0..row.columns().len())
        .map(|index| value_to_cell(row, index))
        .collect()
}

fn value_to_cell(row: &SqliteRow, index: usize) -> Cell {
    match row.try_get_raw(index) {
        Ok(value) if value.is_null() => return None,
        Ok(_) => {}
        Err(_) => return Some("?".to_string()),
    }
    // L'ordre compte : SQLite n'a pas de type par colonne mais un type par
    // valeur, et le premier décodage qui réussit décide de l'affichage. Le
    // texte d'abord, parce qu'un entier stocké en texte doit se lire tel qu'il
    // est écrit.
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
