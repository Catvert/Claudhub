//! What the SQL console offers as one types.
//!
//! The decision lives here, in front of the view that paints it — the pattern
//! of `notes.rs`, `sql_history.rs` and `db::scope`: text and an indexed schema
//! in, a ranked list out, without a line of gpui. The provider in
//! `ui::db_query` is then only a translation into `CompletionItem`s.
//!
//! **The query is read, not just the word being typed.** A three-hundred-table
//! Laravel schema carries thousands of column names, and a list cut at
//! [`MAX`] before anything is ranked can miss the one column the query is
//! about. `db::sql` says which tables the query names and under which alias, so
//! their columns come first — and the schema's foreign keys, indexed already
//! for the grid's links, say which join is possible from there.
//!
//! **Ranked, not restricted.** A half-typed query has no `FROM` yet, and
//! dropping what does not belong to the clause would take away what one is
//! reaching for. Only an explicit qualifier restricts: `u.` offers that table's
//! columns and nothing else.

use std::collections::HashSet;
use std::ops::Range;

use super::link::Key;
use super::sql::{self, Expect, Source};

/// Past this, the list is cut — after ranking, never before.
pub const MAX: usize = 50;

/// Under this many characters, only a prefix matches: one letter is a
/// subsequence of nearly every name, and it would fill the list.
const MIN_FUZZY: usize = 2;

/// Past this, join conditions are not worth offering: they are read one by one.
const MAX_JOINS: usize = 20;

/// What a candidate is, which the view paints as an icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Table,
    Column,
    Keyword,
    /// A whole join condition, written from a foreign key.
    Join,
}

/// One entry of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// What replaces the word being typed.
    pub text: String,
    /// What is read in the list. It is what the prefix matched against, so it
    /// starts with the word one is typing.
    pub label: String,
    /// The greyed-out word beside it: which table a column comes from.
    pub detail: Option<String>,
    pub kind: Kind,
}

/// The word being typed: what precedes the cursor and could be an identifier.
/// It is what a completion replaces.
pub fn word_range(sql: &str, offset: usize) -> Range<usize> {
    let offset = offset.min(sql.len());
    let start = sql[..offset]
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_alphanumeric() || *c == '_')
        .last()
        .map(|(index, _)| index)
        .unwrap_or(offset);
    start..offset
}

/// The query with the word being typed taken out.
///
/// **What one is typing is not a table's alias**, and reading it as one is the
/// bug this exists for: in `FROM users j`, the `j` of a `JOIN` about to be
/// written would name the table `j`, and every condition offered from there
/// would be written against a name that will not survive the next keystroke.
/// The whole identifier goes, the half past the cursor included.
fn without_word(sql: &str, word: &Range<usize>) -> String {
    let end = sql[word.end..]
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map(|index| word.end + index)
        .unwrap_or(sql.len());
    format!("{}{}", &sql[..word.start], &sql[end..])
}

/// The identifier a `.` qualifies the word with: `u.na` is qualified by `u`.
fn qualifier(sql: &str, start: usize) -> Option<String> {
    let before = sql[..start].strip_suffix('.')?;
    let name: String = before
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    let name: String = name.chars().rev().collect();
    (!name.is_empty()).then_some(name)
}

/// The list to offer at `offset`, best first.
pub fn candidates(
    sql: &str,
    offset: usize,
    tables: &[(String, Vec<String>)],
    keys: &[Key],
    keywords: &[&str],
) -> Vec<Candidate> {
    let word = word_range(sql, offset);
    let prefix = &sql[word.clone()];

    // A qualified word names its table, and that is the whole answer: `users.`
    // offers what `users` contains.
    let scan = without_word(sql, &word);
    if let Some(qualifier) = qualifier(sql, word.start) {
        let sources = sql::sources_in(&scan);
        let Some(table) = resolve(&qualifier, &sources, tables) else {
            return Vec::new();
        };
        let Some((_, columns)) = tables.iter().find(|(name, _)| sql::eq(name, &table)) else {
            return Vec::new();
        };
        return rank(
            columns.iter().map(|column| {
                (
                    0,
                    Candidate {
                        text: column.clone(),
                        label: column.clone(),
                        detail: None,
                        kind: Kind::Column,
                    },
                )
            }),
            prefix,
        );
    }

    let expect = sql::expect_at(sql, word.start);
    let sources = sql::sources_in(&scan);
    let mut entries: Vec<(u8, Candidate)> = Vec::new();

    for candidate in joins(&expect, &sources, tables, keys) {
        entries.push((slot_rank(Kind::Join, &expect), candidate));
    }
    let local = slot_rank(Kind::Column, &expect);
    // The rest of the schema, one entry per name: what is not in the query yet
    // is what one is about to name. A column of a table the query names is
    // already in the list, one rank above, and twice over it says nothing.
    //
    // **The names are kept in a set and the prefix filters before the entry is
    // built**: this runs on the interface thread at every keystroke, and a
    // schema of three hundred tables walked with a comparison per name already
    // seen is quadratic. `sql::eq` compares ASCII-case-insensitively, which
    // lowercasing the key reproduces exactly.
    let mut seen: HashSet<String> = HashSet::new();
    for candidate in local_columns(&sources, tables) {
        seen.insert(candidate.label.to_ascii_lowercase());
        if matches(&candidate.label, prefix).is_some() {
            entries.push((local, candidate));
        }
    }
    for (table, columns) in tables {
        if matches(table, prefix).is_some() {
            entries.push((
                slot_rank(Kind::Table, &expect),
                Candidate {
                    text: table.clone(),
                    label: table.clone(),
                    detail: None,
                    kind: Kind::Table,
                },
            ));
        }
        for column in columns {
            // The prefix decides first: two columns of the same name match or
            // miss alike, so what the filter drops never had to be remembered.
            if matches(column, prefix).is_none() {
                continue;
            }
            if !seen.insert(column.to_ascii_lowercase()) {
                continue;
            }
            entries.push((
                local + 1,
                Candidate {
                    text: column.clone(),
                    label: column.clone(),
                    detail: None,
                    kind: Kind::Column,
                },
            ));
        }
    }
    for keyword in keywords {
        if matches(keyword, prefix).is_none() {
            continue;
        }
        entries.push((
            slot_rank(Kind::Keyword, &expect),
            Candidate {
                text: (*keyword).to_string(),
                label: (*keyword).to_string(),
                detail: None,
                kind: Kind::Keyword,
            },
        ));
    }

    rank(entries.into_iter(), prefix)
}

/// Which group comes first, from what the clause expects.
///
/// A column of a table the query names ranks one better than the number this
/// gives — it is the answer to almost every completion asked inside a written
/// query.
fn slot_rank(kind: Kind, expect: &Expect) -> u8 {
    match (expect, kind) {
        (Expect::Table, Kind::Table) => 0,
        (Expect::Table, Kind::Join) => 1,
        (Expect::Table, Kind::Keyword) => 2,
        (Expect::Table, Kind::Column) => 4,

        (Expect::Join(_), Kind::Join) => 0,
        (Expect::Join(_), Kind::Keyword) => 2,
        (Expect::Join(_), Kind::Column) => 4,
        (Expect::Join(_), Kind::Table) => 6,

        (Expect::Column, Kind::Column) => 0,
        (Expect::Column, Kind::Table) => 4,
        (Expect::Column, Kind::Keyword) => 5,
        (Expect::Column, Kind::Join) => 6,

        (Expect::Anything, Kind::Join) => 0,
        (Expect::Anything, Kind::Keyword) => 1,
        (Expect::Anything, Kind::Column) => 2,
        (Expect::Anything, Kind::Table) => 4,
    }
}

/// The name a qualifier stands for: an alias of the query first, a table of the
/// schema otherwise — `u.` and `users.` both have to answer.
fn resolve(
    qualifier: &str,
    sources: &[Source],
    tables: &[(String, Vec<String>)],
) -> Option<String> {
    sources
        .iter()
        .find(|source| sql::eq(source.name(), qualifier))
        .map(|source| source.table.clone())
        .or_else(|| {
            tables
                .iter()
                .find(|(table, _)| sql::eq(table, qualifier))
                .map(|(table, _)| table.clone())
        })
}

/// The columns of the tables the query names, which is what it is about.
///
/// **A name two of them carry goes out qualified**: an unqualified `id` in a
/// two-table join is what the engine refuses as ambiguous, so completing it
/// bare would write a query that cannot run.
fn local_columns(sources: &[Source], tables: &[(String, Vec<String>)]) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    for source in sources {
        let Some((_, columns)) = tables
            .iter()
            .find(|(table, _)| sql::eq(table, &source.table))
        else {
            continue;
        };
        for column in columns {
            let ambiguous = sources.iter().any(|other| {
                !std::ptr::eq(other, source)
                    && tables.iter().any(|(table, columns)| {
                        sql::eq(table, &other.table)
                            && columns.iter().any(|name| sql::eq(name, column))
                    })
            });
            let text = if ambiguous {
                format!("{}.{}", source.name(), column)
            } else {
                column.clone()
            };
            candidates.push(Candidate {
                text,
                label: column.clone(),
                detail: (sources.len() > 1).then(|| source.name().to_string()),
                kind: Kind::Column,
            });
        }
    }
    candidates
}

/// The join conditions the schema's foreign keys allow from here.
///
/// Three shapes, one per place the cursor can be, and each starts with the word
/// one would be typing there — so the prefix filters them like anything else:
/// `ON …` after a named join, `posts ON …` after a bare `JOIN`, and the whole
/// `JOIN posts ON …` anywhere else.
fn joins(
    expect: &Expect,
    sources: &[Source],
    tables: &[(String, Vec<String>)],
    keys: &[Key],
) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    match expect {
        // The table is named: only its condition is left to write.
        Expect::Join(joined) => {
            for other in sources {
                if sql::eq(other.name(), joined.name()) {
                    continue;
                }
                for (left, right) in conditions(joined, other, keys) {
                    candidates.push(Candidate {
                        text: format!("ON {left} = {right}"),
                        label: format!("ON {left} = {right}"),
                        detail: None,
                        kind: Kind::Join,
                    });
                }
            }
        }
        // No table named yet: the table and its condition go in together.
        Expect::Table | Expect::Anything if !sources.is_empty() => {
            let prelude = if matches!(expect, Expect::Table) {
                ""
            } else {
                "JOIN "
            };
            for (table, _) in tables {
                if sources.iter().any(|source| sql::eq(&source.table, table)) {
                    continue;
                }
                let joined = Source {
                    table: table.clone(),
                    alias: None,
                };
                for other in sources {
                    for (left, right) in conditions(&joined, other, keys) {
                        let text = format!("{prelude}{table} ON {left} = {right}");
                        candidates.push(Candidate {
                            text: text.clone(),
                            label: text,
                            detail: None,
                            kind: Kind::Join,
                        });
                    }
                }
            }
        }
        _ => {}
    }
    candidates.truncate(MAX_JOINS);
    candidates
}

/// The two sides of a condition joining `joined` to `other`, whichever of the
/// two carries the key. The joined table is written first: it is the one being
/// added, and the eye reads the condition from it.
fn conditions(joined: &Source, other: &Source, keys: &[Key]) -> Vec<(String, String)> {
    let mut sides = Vec::new();
    for key in keys {
        if sql::eq(&key.table, &joined.table) && sql::eq(&key.target.table, &other.table) {
            sides.push((
                format!("{}.{}", joined.name(), key.column),
                format!("{}.{}", other.name(), key.target.column),
            ));
        }
        if sql::eq(&key.table, &other.table) && sql::eq(&key.target.table, &joined.table) {
            sides.push((
                format!("{}.{}", joined.name(), key.target.column),
                format!("{}.{}", other.name(), key.column),
            ));
        }
    }
    sides
}

/// Keeps what the prefix matches, best first.
///
/// A prefix match comes before a subsequence one — `crat` finding `created_at`
/// is worth having, and never at the price of hiding what starts with what was
/// typed. The sort is stable, so within a rank the schema's order survives.
fn rank(entries: impl Iterator<Item = (u8, Candidate)>, prefix: &str) -> Vec<Candidate> {
    let mut matched: Vec<(u8, u8, Candidate)> = entries
        .filter_map(|(slot, candidate)| {
            let rank = matches(&candidate.label, prefix)?;
            Some((slot, rank, candidate))
        })
        .collect();
    matched.sort_by_key(|(slot, rank, _)| (*slot, *rank));
    matched.truncate(MAX);
    matched
        .into_iter()
        .map(|(_, _, candidate)| candidate)
        .collect()
}

/// How well the label matches the prefix: 0 for a prefix, 1 for a subsequence.
///
/// The comparison is on bytes, ASCII-case-insensitively, and never through a
/// `to_lowercase()`: it changes the byte length of some characters, and a
/// length compared against an offset is how a search lands mid-character.
fn matches(label: &str, prefix: &str) -> Option<u8> {
    if prefix.is_empty() {
        return Some(0);
    }
    let length = prefix.len();
    if label.len() >= length && label.as_bytes()[..length].eq_ignore_ascii_case(prefix.as_bytes()) {
        return Some(0);
    }
    if prefix.len() < MIN_FUZZY {
        return None;
    }
    let mut wanted = prefix.chars();
    let mut next = wanted.next();
    for c in label.chars() {
        match next {
            Some(want) if want.eq_ignore_ascii_case(&c) => next = wanted.next(),
            Some(_) => {}
            None => break,
        }
    }
    next.is_none().then_some(1)
}

/// The keywords offered beside the names, and alone when no schema is indexed.
pub const KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "JOIN",
    "LEFT",
    "RIGHT",
    "INNER",
    "OUTER",
    "CROSS",
    "ON",
    "USING",
    "GROUP BY",
    "ORDER BY",
    "ASC",
    "DESC",
    "LIMIT",
    "OFFSET",
    "HAVING",
    "DISTINCT",
    "AS",
    "AND",
    "OR",
    "NOT",
    "NULL",
    "IS",
    "IN",
    "LIKE",
    "BETWEEN",
    "EXISTS",
    "UNION",
    "ALL",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "COUNT",
    "SUM",
    "AVG",
    "MIN",
    "MAX",
    "INSERT INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE",
    "CREATE",
    "TABLE",
    "ALTER",
    "DROP",
    "INDEX",
    "VIEW",
    "EXPLAIN",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::link::Target;

    fn schema() -> Vec<(String, Vec<String>)> {
        vec![
            (
                "users".into(),
                vec!["id".into(), "email".into(), "created_at".into()],
            ),
            (
                "posts".into(),
                vec!["id".into(), "user_id".into(), "title".into()],
            ),
            (
                "comments".into(),
                vec!["id".into(), "post_id".into(), "body".into()],
            ),
        ]
    }

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
                column: "post_id".into(),
                target: Target {
                    table: "posts".into(),
                    column: "id".into(),
                },
            },
        ]
    }

    fn at(sql: &str) -> Vec<Candidate> {
        let offset = sql.find('|').expect("le curseur est marqué d'un |");
        let sql = sql.replace('|', "");
        candidates(&sql, offset, &schema(), &keys(), KEYWORDS)
    }

    fn labels(sql: &str) -> Vec<String> {
        at(sql)
            .into_iter()
            .map(|candidate| candidate.label)
            .collect()
    }

    /// What the query is about comes first: a column of a table it names, not
    /// the same name borrowed from a table it does not.
    #[test]
    fn the_columns_of_the_named_table_come_first() {
        let names = labels("SELECT | FROM posts");
        assert_eq!(&names[..3], &["id", "user_id", "title"]);
    }

    /// A `FROM` wants a table, and the list says so before it says anything
    /// else.
    #[test]
    fn a_from_offers_tables_first() {
        let names = labels("SELECT * FROM |");
        assert!(
            names.starts_with(&["users".to_string()]),
            "{names:?} ne commence pas par les tables"
        );
    }

    /// An alias answers where the table name does: `u.` is how one writes it.
    #[test]
    fn an_alias_qualifies_as_the_table_does() {
        assert_eq!(
            labels("SELECT u.| FROM users u"),
            vec!["id", "email", "created_at"]
        );
        assert_eq!(
            labels("SELECT users.| FROM users"),
            vec!["id", "email", "created_at"]
        );
        // A qualifier that names nothing offers nothing, rather than the whole
        // schema under a name it does not have.
        assert!(labels("SELECT x.| FROM users u").is_empty());
    }

    /// A name two of the query's tables carry cannot be completed bare: the
    /// engine refuses it as ambiguous.
    #[test]
    fn an_ambiguous_column_is_completed_qualified() {
        let candidates = at("SELECT * FROM users u JOIN posts p ON p.user_id = u.id WHERE i|");
        let id = candidates
            .iter()
            .find(|candidate| candidate.label == "id")
            .expect("la colonne id est proposée");
        assert_eq!(id.text, "u.id");
        assert_eq!(id.detail.as_deref(), Some("u"));

        // What only one of them carries stays bare.
        let candidates = at("SELECT * FROM users u JOIN posts p ON p.user_id = u.id WHERE t|");
        let title = candidates
            .iter()
            .find(|candidate| candidate.label == "title")
            .expect("la colonne title est proposée");
        assert_eq!(title.text, "title");
    }

    /// The whole point of indexing the foreign keys twice over: a join writes
    /// itself.
    #[test]
    fn a_named_join_offers_its_condition() {
        let names = labels("SELECT * FROM users JOIN posts o|");
        assert_eq!(names.first().unwrap(), "ON posts.user_id = users.id");

        // With an alias, the condition is written with it.
        let aliased = labels("SELECT * FROM users u JOIN posts p o|");
        assert_eq!(aliased.first().unwrap(), "ON p.user_id = u.id");
    }

    /// After a bare `JOIN`, the table comes in with its condition — and the
    /// table alone stays right behind it.
    #[test]
    fn a_bare_join_offers_the_table_and_its_condition() {
        let names = labels("SELECT * FROM users JOIN p|");
        assert_eq!(names[0], "posts");
        assert_eq!(names[1], "posts ON posts.user_id = users.id");
    }

    /// Anywhere else, the whole clause goes in — and it is what the `j` one
    /// types is most likely to mean.
    #[test]
    fn a_join_writes_itself_from_a_foreign_key() {
        let names = labels("SELECT * FROM users j|");
        assert_eq!(names[0], "JOIN posts ON posts.user_id = users.id");
        assert_eq!(names[1], "JOIN");
        // A key pointing the other way reads the same.
        let backwards = labels("SELECT * FROM comments j|");
        assert_eq!(backwards[0], "JOIN posts ON posts.id = comments.post_id");
    }

    /// A prefix always beats a subsequence, and a subsequence needs more than
    /// one letter to be one.
    #[test]
    fn a_prefix_comes_before_a_loose_match() {
        assert_eq!(labels("SELECT crat| FROM users")[0], "created_at");

        // A loose match is found, and behind everything a prefix matched.
        let loose = labels("SELECT cdat| FROM users");
        assert!(loose.contains(&"created_at".to_string()));

        let mixed = labels("SELECT cr| FROM users, comments");
        let created = mixed.iter().position(|label| label == "created_at");
        let body = mixed.iter().position(|label| label == "body");
        assert!(
            created < body || body.is_none(),
            "{mixed:?} : le préfixe passe devant le flou"
        );
    }

    /// A one-letter word is a prefix and nothing else: every name is a
    /// subsequence of one letter, and the list would say nothing.
    #[test]
    fn one_letter_matches_by_prefix_only() {
        assert!(
            labels("SELECT b| FROM posts")
                .iter()
                .all(|label| label.to_lowercase().starts_with('b')),
            "une seule lettre ne cherche que des préfixes"
        );
    }

    /// The list is cut after ranking: cutting before is how the column one
    /// meant never shows up.
    #[test]
    fn the_list_is_cut_after_it_is_ranked() {
        let mut tables = schema();
        tables.extend((0..200).map(|index| (format!("table_{index}"), vec!["ident".into()])));
        let sql = "SELECT i FROM posts";
        let candidates = candidates(sql, 8, &tables, &keys(), KEYWORDS);
        assert!(candidates.len() <= MAX);
        assert_eq!(
            candidates[0].label, "id",
            "la colonne de la table nommée passe devant deux cents homonymes"
        );
    }

    /// With no schema indexed there are still the keywords, which is what an
    /// empty console offers.
    #[test]
    fn a_console_without_a_schema_offers_keywords() {
        let candidates = candidates("sel", 3, &[], &[], KEYWORDS);
        assert_eq!(candidates[0].label, "SELECT");
        assert_eq!(candidates[0].kind, Kind::Keyword);
    }

    /// The word being typed is what a completion replaces — the `users.` of a
    /// qualified column is not part of it.
    #[test]
    fn the_replaced_word_stops_at_the_qualifier() {
        assert_eq!(word_range("SELECT users.na", 15), 13..15);
        assert_eq!(word_range("SELECT ", 7), 7..7);
    }
}
