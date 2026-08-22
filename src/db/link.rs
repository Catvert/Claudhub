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
//! **query**: the tables it names after `FROM` and `JOIN`, matched against the
//! schema's foreign keys. That is exact for `SELECT * FROM t`, which is what
//! opening a table from the tree writes, and honest elsewhere — where two of
//! the named tables carry the same column name towards two different targets,
//! nothing is offered rather than something wrong.

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

/// Identifiers compare without case: both engines answer to `Users` and
/// `users`, and a schema written in one and a query typed in the other must
/// still meet.
fn eq(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

/// The tables a query names, after `FROM` and each kind of `JOIN`.
///
/// A rough scan and not a parser: strings, quoted identifiers and comments are
/// skipped so that a `FROM` written inside one is not read as a clause, and
/// what follows a `(` — a subquery — names no table of ours. An alias is simply
/// the word after, which is not read.
pub fn tables_in(sql: &str) -> Vec<String> {
    let mut tables: Vec<String> = Vec::new();
    let mut words = Words::new(sql);
    let mut expect = false;
    while let Some(word) = words.next_word() {
        if expect {
            expect = false;
            if let Some(name) = table_name(&word) {
                if !tables.iter().any(|table| eq(table.as_str(), &name)) {
                    tables.push(name);
                }
            }
            continue;
        }
        expect = word.eq_ignore_ascii_case("from") || word.eq_ignore_ascii_case("join");
    }
    tables
}

/// `db.users` names `users`: what the schema index files is the bare name.
/// A subquery, an opening parenthesis or a keyword names nothing.
fn table_name(word: &str) -> Option<String> {
    let name = word.rsplit('.').next().unwrap_or(word).trim();
    let usable = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$');
    usable.then(|| name.to_string())
}

/// A query's words, with strings, comments and quoted identifiers set aside.
struct Words<'a> {
    rest: &'a str,
}

impl<'a> Words<'a> {
    fn new(sql: &'a str) -> Self {
        Self { rest: sql }
    }

    /// Reads what follows a `.`, and keeps the last part.
    fn qualified(&mut self, word: String) -> String {
        let mut word = word;
        while let Some(rest) = self.rest.strip_prefix('.') {
            self.rest = rest;
            match self.next_word() {
                Some(next) => word = next,
                None => break,
            }
        }
        word
    }

    fn next_word(&mut self) -> Option<String> {
        loop {
            self.rest = self.rest.trim_start();
            let mut chars = self.rest.chars();
            let first = chars.next()?;
            let after = chars.as_str();
            match first {
                // A line comment, a block comment: nothing of ours in there.
                '-' if after.starts_with('-') => {
                    self.rest = after.split_once('\n').map(|(_, rest)| rest).unwrap_or("");
                }
                '/' if after.starts_with('*') => {
                    self.rest = after.split_once("*/").map(|(_, rest)| rest).unwrap_or("");
                }
                '\'' | '"' | '`' => {
                    let Some((word, rest)) = after.split_once(first) else {
                        self.rest = "";
                        return Some(String::new());
                    };
                    self.rest = rest;
                    // A quoted identifier is a word: `FROM "users"` names one.
                    // `` `db`.`posts` `` is one too, and it is its last part
                    // that names the table — the same reading `table_name` does
                    // of an unquoted `db.posts`.
                    return Some(self.qualified(word.to_string()));
                }
                _ if first.is_alphanumeric() || first == '_' || first == '$' || first == '.' => {
                    let end = self
                        .rest
                        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$' || c == '.'))
                        .unwrap_or(self.rest.len());
                    let (word, rest) = self.rest.split_at(end);
                    self.rest = rest;
                    return Some(word.to_string());
                }
                // Anything else — a comma, a parenthesis, an operator — is a
                // separator, and it breaks a `FROM` from what would have
                // followed it.
                _ => {
                    self.rest = after;
                    return Some(first.to_string());
                }
            }
        }
    }
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
    fn a_plain_select_names_its_table() {
        assert_eq!(tables_in("SELECT * FROM posts;"), vec!["posts"]);
        assert_eq!(tables_in("select * from `db`.`posts` p"), vec!["posts"]);
        assert_eq!(
            tables_in("SELECT * FROM posts JOIN users ON users.id = posts.user_id"),
            vec!["posts", "users"]
        );
    }

    #[test]
    fn a_from_inside_a_string_or_a_comment_names_nothing() {
        assert_eq!(
            tables_in("SELECT 'from nowhere' FROM posts -- from elsewhere"),
            vec!["posts"]
        );
        assert_eq!(
            tables_in("SELECT * /* from there */ FROM posts"),
            vec!["posts"]
        );
    }

    #[test]
    fn a_subquery_is_not_a_table() {
        assert_eq!(
            tables_in("SELECT * FROM (SELECT * FROM posts) AS p"),
            vec!["posts"]
        );
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
