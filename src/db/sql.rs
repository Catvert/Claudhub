//! Reading a query as text.
//!
//! What the grid's links and the console's completions both need to know about
//! a query — which tables it names, under which alias, and what the cursor sits
//! in the middle of — and what no engine will answer: sqlx keeps a result
//! column's name, its ordinal and its type, never the table it came from. It is
//! text in, text out, like `db::scope`.
//!
//! **A rough scan and not a parser.** Strings, quoted identifiers and comments
//! are set aside so that a `FROM` written inside one is not read as a clause,
//! and what follows a `(` — a subquery — names no table of ours. That is exact
//! for the queries this console writes and honest for the rest: where the
//! reading is not sure, the caller offers nothing rather than something wrong.

/// A word of a query, and whether it was written between quotes.
///
/// **A quoted word is never a keyword**: `FROM "from"` names a table called
/// `from`, and reading it as a clause loses the table that follows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    pub text: String,
    pub quoted: bool,
}

impl Word {
    /// Whether this word is the given keyword, written in any case.
    pub fn is(&self, keyword: &str) -> bool {
        !self.quoted && self.text.eq_ignore_ascii_case(keyword)
    }

    /// Whether this word is one of the given keywords.
    pub fn any(&self, keywords: &[&str]) -> bool {
        keywords.iter().any(|keyword| self.is(keyword))
    }

    /// The name this word gives a table: `db.users` names `users`, which is
    /// what the schema index files. A separator or a keyword names nothing.
    pub fn name(&self) -> Option<String> {
        let name = self.text.rsplit('.').next().unwrap_or(&self.text).trim();
        let usable = !name.is_empty() && name.chars().all(is_identifier);
        usable.then(|| name.to_string())
    }
}

/// A table the query names, under the alias it was given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub table: String,
    /// `FROM users u` — what the rest of the query calls the table.
    pub alias: Option<String>,
}

impl Source {
    /// What a qualified column of this table is written with: the alias when
    /// there is one, the table itself otherwise.
    pub fn name(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.table)
    }
}

/// What the cursor is in the middle of, which says what to offer first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    /// The start of a statement, or a place a keyword follows.
    Anything,
    /// After `FROM`, after `JOIN`, or after a comma in a `FROM` list.
    Table,
    /// A `SELECT`, `WHERE`, `ON` or `BY` list.
    Column,
    /// A `JOIN` whose table is named and whose `ON` has not come yet — the one
    /// place a whole join condition can be offered.
    Join(Source),
}

/// Characters an unquoted identifier is made of.
fn is_identifier(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Identifiers compare without case: both engines answer to `Users` and
/// `users`, and a schema written in one and a query typed in the other must
/// still meet.
pub fn eq(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

/// Keywords that end a `FROM` list, so that what follows a comma is a column
/// again.
const OUT_OF_FROM: &[&str] = &[
    "select",
    "where",
    "on",
    "using",
    "group",
    "order",
    "having",
    "limit",
    "offset",
    "set",
    "values",
    "union",
    "returning",
];

/// What cannot be a table's alias: the word after a table is only an alias when
/// it is not the start of the next clause.
const NOT_AN_ALIAS: &[&str] = &[
    "on",
    "using",
    "where",
    "join",
    "left",
    "right",
    "inner",
    "outer",
    "cross",
    "natural",
    "group",
    "order",
    "having",
    "limit",
    "offset",
    "union",
    "set",
    "values",
    "returning",
    "and",
    "or",
];

/// Words that put the cursor back in a list of columns.
const COLUMN_CLAUSES: &[&str] = &[
    "select",
    "on",
    "using",
    "where",
    "and",
    "or",
    "having",
    "by",
    "set",
    "returning",
    "distinct",
];

/// The words of a query, with strings, comments and quoted identifiers set
/// aside.
pub fn words(sql: &str) -> Vec<Word> {
    let mut words = Vec::new();
    let mut rest = sql;
    loop {
        rest = rest.trim_start();
        let mut chars = rest.chars();
        let Some(first) = chars.next() else {
            return words;
        };
        let after = chars.as_str();
        match first {
            // A line comment, a block comment: nothing of ours in there.
            '-' if after.starts_with('-') => {
                rest = after.split_once('\n').map(|(_, rest)| rest).unwrap_or("");
            }
            '/' if after.starts_with('*') => {
                rest = after.split_once("*/").map(|(_, rest)| rest).unwrap_or("");
            }
            '\'' | '"' | '`' => {
                let Some((word, tail)) = after.split_once(first) else {
                    return words;
                };
                rest = tail;
                // `` `db`.`posts` `` names one table, and it is its last part
                // that names it — the same reading `Word::name` does of an
                // unquoted `db.posts`. Each part may be quoted in its turn.
                let mut word = word.to_string();
                while let Some(tail) = rest.strip_prefix('.') {
                    let (next, tail) = read_part(tail);
                    match next {
                        Some(next) => {
                            word = next;
                            rest = tail;
                        }
                        None => break,
                    }
                }
                words.push(Word {
                    text: word,
                    quoted: true,
                });
            }
            _ => {
                let (word, tail) = read_word(rest);
                rest = tail;
                match word {
                    Some(word) => words.push(word),
                    None => return words,
                }
            }
        }
    }
}

/// One part of a qualified name, quoted or not.
fn read_part(rest: &str) -> (Option<String>, &str) {
    let mut chars = rest.chars();
    match chars.next() {
        Some(quote @ ('\'' | '"' | '`')) => match chars.as_str().split_once(quote) {
            Some((word, tail)) => (Some(word.to_string()), tail),
            None => (None, ""),
        },
        _ => {
            let (word, tail) = read_word(rest);
            (word.map(|word| word.text), tail)
        }
    }
}

/// One unquoted word, or the single character that separates two.
fn read_word(rest: &str) -> (Option<Word>, &str) {
    let mut chars = rest.chars();
    let Some(first) = chars.next() else {
        return (None, "");
    };
    if is_identifier(first) || first == '.' {
        let end = rest
            .find(|c: char| !(is_identifier(c) || c == '.'))
            .unwrap_or(rest.len());
        let (word, tail) = rest.split_at(end);
        return (
            Some(Word {
                text: word.to_string(),
                quoted: false,
            }),
            tail,
        );
    }
    // A comma, a parenthesis, an operator: a separator, and it breaks a `FROM`
    // from what would have followed it.
    (
        Some(Word {
            text: first.to_string(),
            quoted: false,
        }),
        chars.as_str(),
    )
}

/// The tables a query names, after `FROM` and each kind of `JOIN`, with their
/// aliases.
///
/// A self-join names the same table twice, so nothing is deduplicated here:
/// what tells the two apart is precisely the alias.
pub fn sources_in(sql: &str) -> Vec<Source> {
    let words = words(sql);
    let mut sources = Vec::new();
    let mut index = 0;
    let mut expect = false;
    let mut in_from = false;
    while index < words.len() {
        let word = &words[index];
        index += 1;
        if expect {
            expect = false;
            if let Some(table) = word.name() {
                let (alias, next) = alias_after(&words, index);
                index = next;
                sources.push(Source { table, alias });
            }
            continue;
        }
        if word.is("from") || word.is("join") {
            expect = true;
            in_from = true;
        } else if word.any(OUT_OF_FROM) {
            in_from = false;
        } else if word.text == "," && in_from {
            expect = true;
        }
    }
    sources
}

/// The alias written after a table, and where reading resumes.
///
/// `AS` is consumed when it is there; without it the alias is simply the word
/// after, and only when that word is not the start of the next clause — `FROM
/// posts JOIN` would otherwise call the table `join`.
fn alias_after(words: &[Word], index: usize) -> (Option<String>, usize) {
    let mut index = index;
    if words.get(index).is_some_and(|word| word.is("as")) {
        index += 1;
    } else if words
        .get(index)
        .is_none_or(|word| word.any(NOT_AN_ALIAS) || word.name().is_none())
    {
        return (None, index);
    }
    match words.get(index).and_then(Word::name) {
        Some(alias) => (Some(alias), index + 1),
        None => (None, index),
    }
}

/// The tables a query names, deduplicated, as the links read them.
pub fn tables_in(sql: &str) -> Vec<String> {
    let mut tables: Vec<String> = Vec::new();
    for source in sources_in(sql) {
        if !tables.iter().any(|table| eq(table, &source.table)) {
            tables.push(source.table);
        }
    }
    tables
}

/// What the query expects at `upto`, read from what precedes it.
///
/// Only the text **before** the cursor is read: what follows belongs to a
/// clause one has not come back to yet.
pub fn expect_at(sql: &str, upto: usize) -> Expect {
    let upto = upto.min(sql.len());
    let words = words(&sql[..upto]);
    let mut expect = Expect::Anything;
    let mut in_from = false;
    let mut joining = false;
    let mut index = 0;
    while index < words.len() {
        let word = &words[index];
        index += 1;
        if word.text == ";" {
            expect = Expect::Anything;
            in_from = false;
            joining = false;
            continue;
        }
        if word.is("from") || word.is("join") {
            joining = word.is("join");
            in_from = true;
            expect = Expect::Table;
            continue;
        }
        if word.any(COLUMN_CLAUSES) {
            in_from = false;
            joining = false;
            expect = Expect::Column;
            continue;
        }
        if word.any(OUT_OF_FROM) || word.any(NOT_AN_ALIAS) {
            in_from = false;
            expect = Expect::Anything;
            continue;
        }
        if word.text == "," {
            expect = if in_from {
                Expect::Table
            } else {
                Expect::Column
            };
            continue;
        }
        if word.text == "(" {
            // `count(` lists columns; `FROM (` opens a subquery, which starts
            // over.
            expect = match expect {
                Expect::Table | Expect::Anything => Expect::Anything,
                _ => Expect::Column,
            };
            continue;
        }
        if !matches!(expect, Expect::Table) {
            continue;
        }
        // The word a `FROM` or a `JOIN` was waiting for.
        let Some(table) = word.name() else {
            expect = Expect::Anything;
            continue;
        };
        let (alias, next) = alias_after(&words, index);
        index = next;
        let source = Source { table, alias };
        expect = if joining {
            Expect::Join(source)
        } else {
            Expect::Anything
        };
        joining = false;
    }
    expect
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(table: &str, alias: Option<&str>) -> Source {
        Source {
            table: table.into(),
            alias: alias.map(str::to_string),
        }
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
        // A quoted word is never a keyword, so the table after it is not lost.
        assert_eq!(tables_in("SELECT 'from' FROM posts"), vec!["posts"]);
    }

    #[test]
    fn a_subquery_is_not_a_table() {
        assert_eq!(
            tables_in("SELECT * FROM (SELECT * FROM posts) AS p"),
            vec!["posts"]
        );
    }

    /// The alias is what the rest of the query writes its columns with, and the
    /// word after a table is only one when it does not open the next clause.
    #[test]
    fn a_table_carries_the_alias_it_was_given() {
        assert_eq!(
            sources_in("SELECT * FROM users u JOIN posts AS p ON p.user_id = u.id"),
            vec![source("users", Some("u")), source("posts", Some("p"))]
        );
        assert_eq!(
            sources_in("SELECT * FROM users JOIN posts ON posts.user_id = users.id"),
            vec![source("users", None), source("posts", None)]
        );
        assert_eq!(
            sources_in("SELECT * FROM users WHERE id = 1"),
            vec![source("users", None)]
        );
        assert_eq!(
            sources_in("SELECT * FROM users LEFT JOIN posts ON posts.user_id = users.id"),
            vec![source("users", None), source("posts", None)]
        );
    }

    /// A self-join names one table twice, and the alias is the only thing that
    /// tells the two apart.
    #[test]
    fn a_self_join_is_two_sources() {
        assert_eq!(
            sources_in("SELECT * FROM users u JOIN users m ON m.id = u.manager_id"),
            vec![source("users", Some("u")), source("users", Some("m"))]
        );
        assert_eq!(
            tables_in("SELECT * FROM users u JOIN users m"),
            vec!["users"]
        );
    }

    /// A comma lists tables inside a `FROM` and columns everywhere else.
    #[test]
    fn a_comma_lists_what_the_clause_lists() {
        assert_eq!(
            sources_in("SELECT id, name FROM users, posts"),
            vec![source("users", None), source("posts", None)]
        );
        assert_eq!(expect_at("SELECT id, ", 10), Expect::Column);
        assert_eq!(
            expect_at("SELECT * FROM users, ", 21),
            Expect::Table,
            "la virgule d'un FROM attend une table"
        );
    }

    #[test]
    fn the_cursor_knows_which_clause_it_is_in() {
        assert_eq!(expect_at("", 0), Expect::Anything);
        assert_eq!(expect_at("SELECT ", 7), Expect::Column);
        assert_eq!(expect_at("SELECT * FROM ", 14), Expect::Table);
        assert_eq!(expect_at("SELECT * FROM users ", 20), Expect::Anything);
        assert_eq!(expect_at("SELECT * FROM users WHERE ", 26), Expect::Column);
        assert_eq!(
            expect_at("SELECT * FROM users ORDER BY ", 29),
            Expect::Column
        );
        assert_eq!(
            expect_at("SELECT * FROM users; SELECT ", 28),
            Expect::Column,
            "un point-virgule recommence"
        );
    }

    /// The one place a whole join condition can be offered: a `JOIN` whose
    /// table is named and whose `ON` has not come yet.
    #[test]
    fn a_named_join_waits_for_its_condition() {
        assert_eq!(
            expect_at("SELECT * FROM users JOIN posts ", 31),
            Expect::Join(source("posts", None))
        );
        assert_eq!(
            expect_at("SELECT * FROM users JOIN posts p ", 33),
            Expect::Join(source("posts", Some("p")))
        );
        assert_eq!(
            expect_at("SELECT * FROM users JOIN posts ON ", 34),
            Expect::Column,
            "la condition a commencé"
        );
        assert_eq!(
            expect_at("SELECT * FROM users JOIN ", 25),
            Expect::Table,
            "la table n'est pas encore nommée"
        );
    }

    /// The text after the cursor belongs to a clause one has not come back to.
    #[test]
    fn only_what_precedes_the_cursor_is_read() {
        let sql = "SELECT  FROM users";
        assert_eq!(expect_at(sql, 7), Expect::Column);
    }
}
