//! Following a foreign key from a result cell.
//!
//! A result grid names rows by their keys, and reading one means going to the
//! row it points at. All that is decided here — which column of a result is a
//! foreign key, and what query goes and fetches the row it names — and none of
//! it touches gpui or the network: it is text in, text out, like `db::scope`.
//!
//! **The engine does not say which table a result column comes from.** MySQL's
//! protocol carries `org_table` in its column definitions, but sqlx keeps only
//! the name, the ordinal and the type. The source is therefore read from the
//! **query** — `db::sql`, the same scan the console completes with: the tables
//! it names after `FROM` and `JOIN`, matched against the schema's foreign keys.
//! That is exact for `SELECT * FROM t`, which is what opening a table from the
//! tree writes, and honest elsewhere — where two of the named tables carry the
//! same column name towards two different targets, nothing is offered rather
//! than something wrong.

use super::sql::{eq, tables_in};
use super::Engine;

/// A foreign key of the indexed schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub table: String,
    pub column: String,
    pub target: Target,
}

/// What a foreign key points at: one column of one table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub table: String,
    pub column: String,
}

impl Target {
    /// `users.id`, as the menu entry names it.
    pub fn label(&self) -> String {
        format!("{}.{}", self.table, self.column)
    }
}

/// The schema's foreign keys, as the column index carries them.
pub fn keys_of(columns: &std::collections::BTreeMap<String, Vec<super::Column>>) -> Vec<Key> {
    let mut keys = Vec::new();
    for (table, columns) in columns {
        for column in columns {
            // `table.column`, as both engines write it. Split from the right:
            // it is the column that is one word for sure.
            let Some((target_table, target_column)) = column
                .foreign_key
                .as_deref()
                .and_then(|target| target.rsplit_once('.'))
            else {
                continue;
            };
            keys.push(Key {
                table: table.clone(),
                column: column.name.clone(),
                target: Target {
                    table: target_table.to_string(),
                    column: target_column.to_string(),
                },
            });
        }
    }
    keys
}

/// Which of a result's columns can be followed, one entry per column.
///
/// Computed once, when the rows arrive, and never in a render closure: the
/// table asks for its cells one by one on every frame.
pub fn targets(sql: &str, columns: &[String], keys: &[Key]) -> Vec<Option<Target>> {
    let tables = tables_in(sql);
    columns
        .iter()
        .map(|column| {
            let mut found: Option<&Target> = None;
            for key in keys {
                if !eq(&key.column, column)
                    || !tables.iter().any(|table| eq(table.as_str(), &key.table))
                {
                    continue;
                }
                match found {
                    // Two of the query's tables carry this name and do not point
                    // at the same row. Which one the value belongs to cannot be
                    // known from here, and guessing would send the reader to
                    // another table's row.
                    Some(target) if target != &key.target => return None,
                    _ => found = Some(&key.target),
                }
            }
            found.cloned()
        })
        .collect()
}

/// The query that fetches the row a value names.
pub fn select_row(engine: Engine, target: &Target, value: &str) -> String {
    format!(
        "SELECT * FROM {} WHERE {} = {};",
        quote(engine, &target.table),
        quote(engine, &target.column),
        literal(engine, value)
    )
}

/// An identifier, quoted the way the engine writes them.
pub fn quote(engine: Engine, name: &str) -> String {
    match engine {
        Engine::Mysql => format!("`{}`", name.replace('`', "``")),
        Engine::Sqlite => format!("\"{}\"", name.replace('"', "\"\"")),
    }
}

/// A value, as a literal of the engine.
///
/// **A number goes out bare**, and that is not a nicety: SQLite compares an
/// `INTEGER` to a `'42'` by type before value and finds nothing, so a quoted key
/// would silently return an empty result. What is not a number is quoted, with
/// the quote doubled — and the backslash doubled too on MySQL, which reads it as
/// an escape inside a string where SQLite does not.
pub fn literal(engine: Engine, value: &str) -> String {
    if is_number(value) {
        return value.to_string();
    }
    let escaped = match engine {
        Engine::Mysql => value.replace('\\', "\\\\").replace('\'', "''"),
        Engine::Sqlite => value.replace('\'', "''"),
    };
    format!("'{escaped}'")
}

fn is_number(value: &str) -> bool {
    let body = value.strip_prefix('-').unwrap_or(value);
    if body.is_empty() {
        return false;
    }
    let mut parts = body.splitn(2, '.');
    let whole = parts.next().unwrap_or_default();
    let fraction = parts.next();
    !whole.is_empty()
        && whole.bytes().all(|byte| byte.is_ascii_digit())
        && fraction.is_none_or(|fraction| {
            !fraction.is_empty() && fraction.bytes().all(|byte| byte.is_ascii_digit())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> Vec<Key> {
        vec![
            Key {
                table: "posts".into(),
                column: "user_id".into(),
                target: Target {
                    table: "users".into(),
                    column: "id".into(),
                },
            },
            Key {
                table: "comments".into(),
                column: "user_id".into(),
                target: Target {
                    table: "users".into(),
                    column: "id".into(),
                },
            },
            Key {
                table: "comments".into(),
                column: "post_id".into(),
                target: Target {
                    table: "posts".into(),
                    column: "id".into(),
                },
            },
        ]
    }

    #[test]
    fn a_key_of_the_named_table_is_followed() {
        let columns = vec!["id".to_string(), "user_id".to_string()];
        let targets = targets("SELECT * FROM posts", &columns, &keys());
        assert_eq!(targets[0], None);
        assert_eq!(
            targets[1].as_ref().map(Target::label).as_deref(),
            Some("users.id")
        );
    }

    #[test]
    fn a_table_the_query_does_not_name_is_ignored() {
        let columns = vec!["user_id".to_string()];
        assert_eq!(targets("SELECT * FROM logs", &columns, &keys()), vec![None]);
    }

    #[test]
    fn two_tables_pointing_at_the_same_row_stay_unambiguous() {
        let columns = vec!["user_id".to_string()];
        let sql = "SELECT * FROM posts JOIN comments ON comments.post_id = posts.id";
        assert_eq!(
            targets(sql, &columns, &keys())[0]
                .as_ref()
                .map(Target::label)
                .as_deref(),
            Some("users.id")
        );
    }

    #[test]
    fn two_tables_pointing_elsewhere_offer_nothing() {
        let mut keys = keys();
        keys[1].target.table = "accounts".into();
        let columns = vec!["user_id".to_string()];
        let sql = "SELECT * FROM posts JOIN comments ON comments.post_id = posts.id";
        assert_eq!(targets(sql, &columns, &keys), vec![None]);
    }

    #[test]
    fn a_number_goes_out_bare_and_the_rest_quoted() {
        assert_eq!(literal(Engine::Mysql, "42"), "42");
        assert_eq!(literal(Engine::Mysql, "-3.5"), "-3.5");
        assert_eq!(literal(Engine::Mysql, "007a"), "'007a'");
        assert_eq!(literal(Engine::Mysql, "l'ami"), "'l''ami'");
        assert_eq!(literal(Engine::Mysql, "a\\b"), "'a\\\\b'");
        assert_eq!(literal(Engine::Sqlite, "a\\b"), "'a\\b'");
    }

    #[test]
    fn the_query_quotes_the_way_the_engine_does() {
        let target = Target {
            table: "users".into(),
            column: "id".into(),
        };
        assert_eq!(
            select_row(Engine::Mysql, &target, "42"),
            "SELECT * FROM `users` WHERE `id` = 42;"
        );
        assert_eq!(
            select_row(Engine::Sqlite, &target, "a"),
            "SELECT * FROM \"users\" WHERE \"id\" = 'a';"
        );
    }
}
