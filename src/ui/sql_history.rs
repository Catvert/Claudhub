//! Every query run, kept per worktree.
//!
//! The console remembered the query it was showing and nothing else: what one
//! had written the day before, in the worktree next door, against the tenant
//! whose name one no longer recalls, was gone the moment the editor was
//! cleared. A SQL console without a history is a console where one rewrites the
//! same `SELECT` every afternoon.
//!
//! **Per worktree, because that is where the work is.** A worktree is a branch,
//! a set of cloned databases and a question being answered; the same query
//! typed while reviewing another branch belongs to that other review. Nothing
//! is walled off for all that — the panel's reach selector reads the whole file
//! — but what one sees on arriving is what one was doing here.
//!
//! **A file of its own** (`<config>/sql_history.json`), not the state store.
//! The store is "where one is at": a kilobyte, rewritten in full every half
//! second while one types in the SQL editor — the console's text goes through
//! it on every keystroke. A cumulative journal of several hundred kilobytes has
//! no business being reserialised at that rhythm.
//!
//! This module knows nothing of gpui: it holds the model, the deduplication,
//! the search and the day grouping, and it is tested. `sql_history_view.rs` is
//! the plumbing, as `notes.rs` is to `notes_view.rs`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How many queries a worktree keeps.
///
/// A ceiling and not a total: it is per worktree, because the point is to find
/// again what was done *here*, and a project queried all afternoon must not
/// push out the three queries of the worktree next door. Two hundred is a
/// fortnight of ordinary use, and thirty kilobytes.
pub const PER_WORKTREE: usize = 200;

/// One query, as it went out and as it came back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Entry {
    /// Unix seconds. A number and not a formatted date: it sorts, it survives a
    /// change of locale, and the panel formats it as it likes.
    pub at: i64,
    /// The checkout the console belonged to.
    pub worktree: PathBuf,
    /// The connection's key (`db::Connection::key`), which names it without
    /// carrying its password — the store's rule for the same reason.
    pub connection: String,
    /// The connection's label at the time, so a row still says something about
    /// a connection since removed from the settings.
    pub label: String,
    pub database: Option<String>,
    /// The query as it was written, never the one paging and sorting rewrite
    /// around it: what one wants back in the editor is what one typed.
    pub sql: String,
    /// It came back without an error.
    pub ok: bool,
    /// Rows in the first window, or what a write touched. Both are what the bar
    /// said, and it is what tells a query apart from the one beside it.
    pub rows: Option<usize>,
    pub affected: Option<u64>,
    /// The engine's message, first line only: a row is one line tall, and a
    /// stack trace in a tooltip is not read either.
    pub error: Option<String>,
    pub elapsed_ms: u64,
    /// How many times it has been run — see `record`. Never zero.
    pub runs: u32,
}

impl Default for Entry {
    fn default() -> Self {
        Self {
            at: 0,
            worktree: PathBuf::new(),
            connection: String::new(),
            label: String::new(),
            database: None,
            sql: String::new(),
            ok: true,
            rows: None,
            affected: None,
            error: None,
            elapsed_ms: 0,
            runs: 1,
        }
    }
}

impl Entry {
    /// The first non-empty line, which is what a row shows.
    ///
    /// A twenty-line query is read in the editor, not in a list: what the list
    /// owes is enough to recognise it.
    pub fn headline(&self) -> &str {
        self.sql
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("")
    }

    /// The query has more to it than the line shown.
    pub fn is_multiline(&self) -> bool {
        self.sql
            .trim()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count()
            > 1
    }

    /// What identifies "the same query, run again": the text, where it was run,
    /// and against what. Whitespace is normalised — reindenting a query does not
    /// make it another one.
    fn identity(&self) -> (String, &Path, &str, Option<&str>) {
        (
            normalise(&self.sql),
            self.worktree.as_path(),
            self.connection.as_str(),
            self.database.as_deref(),
        )
    }
}

/// Collapses runs of whitespace, so indentation does not make a duplicate.
fn normalise(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Which entries the panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Reach {
    /// What was run in the checkout being looked at. The default, and the point
    /// of the whole thing.
    #[default]
    Worktree,
    /// What was run against the console's connection, whatever the worktree: the
    /// same database is queried from five checkouts.
    Connection,
    /// Everything kept.
    All,
}

impl Reach {
    pub const ALL: [Reach; 3] = [Reach::Worktree, Reach::Connection, Reach::All];
}

/// What the panel asks the history for.
#[derive(Debug, Clone, Copy, Default)]
pub struct Filter<'a> {
    pub reach: Reach,
    /// The selected checkout. `None` — nothing selected yet — shows everything
    /// rather than nothing: an empty list would read as a lost history.
    pub worktree: Option<&'a Path>,
    /// The console's connection, for `Reach::Connection`.
    pub connection: Option<&'a str>,
    pub query: &'a str,
}

/// A row of the list: a day's heading, or one entry.
///
/// Headings are part of the list and not sections around it, because the list
/// is virtualised: one flat vector of rows is what a virtual list walks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Day(Day),
    /// An index into what `matching` returned, never into `entries`: a filtered
    /// list has its own numbering.
    Entry(usize),
}

/// A day, as the heading names it.
///
/// Today and yesterday are said in words — that is how one reads them — and the
/// rest by its date. The wording is the view's, the decision is here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Day {
    Today,
    Yesterday,
    /// `YYYY-MM-DD`, which sorts and does not depend on a locale.
    On(String),
}

/// The journal.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct History {
    /// Most recent first. That is the order it is read in, and keeping it here
    /// spares a sort on every render.
    pub entries: Vec<Entry>,
}

impl History {
    /// Files a query that has just come back.
    ///
    /// **The same query run again does not make a second row.** One pages, one
    /// corrects a typo, one runs the same `SELECT` four times in a row while
    /// looking at the data change: four identical rows say nothing more than
    /// one, and they push out the query from this morning that one is actually
    /// looking for. The existing row moves back to the top, counts one more run
    /// and takes the fresh result — what a row says about rows and duration must
    /// be about the last time it was run, not the first.
    pub fn record(&mut self, entry: Entry) {
        let identity = entry.identity();
        if let Some(index) = self
            .entries
            .iter()
            .position(|other| other.identity() == identity)
        {
            let previous = self.entries.remove(index);
            let mut entry = entry;
            entry.runs = previous.runs.saturating_add(1);
            self.entries.insert(0, entry);
        } else {
            self.entries.insert(0, entry);
        }
        self.purge();
    }

    /// Keeps each worktree's last `PER_WORKTREE` queries.
    fn purge(&mut self) {
        let mut kept: std::collections::HashMap<PathBuf, usize> = std::collections::HashMap::new();
        self.entries.retain(|entry| {
            let count = kept.entry(entry.worktree.clone()).or_default();
            *count += 1;
            *count <= PER_WORKTREE
        });
    }

    /// Forgets one query.
    pub fn remove(&mut self, at: i64, sql: &str) {
        self.entries
            .retain(|entry| !(entry.at == at && entry.sql == sql));
    }

    /// Forgets what the filter shows, and only that.
    ///
    /// Clearing from a panel showing one worktree must not empty the others:
    /// what one asks to forget is what one is looking at.
    pub fn clear(&mut self, filter: &Filter) {
        self.entries.retain(|entry| !keeps(filter, entry));
    }

    /// The entries the filter keeps, most recent first.
    pub fn matching(&self, filter: &Filter) -> Vec<&Entry> {
        self.entries
            .iter()
            .filter(|entry| keeps(filter, entry))
            .collect()
    }

    /// How many queries this checkout has kept, for the panel's bar.
    pub fn count_for(&self, worktree: Option<&Path>) -> usize {
        match worktree {
            Some(path) => self
                .entries
                .iter()
                .filter(|entry| entry.worktree == path)
                .count(),
            None => self.entries.len(),
        }
    }
}

/// Does an entry pass the filter?
///
/// The search covers the query **and** the database name: "which of these was
/// on the itcs tenant" is exactly the question one comes here with.
fn keeps(filter: &Filter, entry: &Entry) -> bool {
    let reached = match filter.reach {
        Reach::All => true,
        Reach::Worktree => filter.worktree.is_none_or(|path| entry.worktree == path),
        Reach::Connection => filter.connection.is_none_or(|key| entry.connection == key),
    };
    reached
        && (crate::ui::find::matches(filter.query, &entry.sql)
            || crate::ui::find::matches(filter.query, entry.database.as_deref().unwrap_or("")))
}

/// Lays the filtered entries out under their day.
///
/// `today` is passed in rather than read: this function is pure, which is what
/// makes "yesterday" testable at all.
pub fn rows(entries: &[&Entry], today: chrono::NaiveDate) -> Vec<Row> {
    let mut out = Vec::with_capacity(entries.len() + 4);
    let mut current: Option<chrono::NaiveDate> = None;
    for (index, entry) in entries.iter().enumerate() {
        let date = date_of(entry.at);
        if current != Some(date) {
            out.push(Row::Day(day_of(date, today)));
            current = Some(date);
        }
        out.push(Row::Entry(index));
    }
    out
}

fn date_of(at: i64) -> chrono::NaiveDate {
    chrono::DateTime::from_timestamp(at, 0)
        .map(|utc| utc.with_timezone(&chrono::Local).date_naive())
        .unwrap_or_default()
}

fn day_of(date: chrono::NaiveDate, today: chrono::NaiveDate) -> Day {
    match (today - date).num_days() {
        0 => Day::Today,
        1 => Day::Yesterday,
        _ => Day::On(date.format("%Y-%m-%d").to_string()),
    }
}

/// The hour a row shows, in the local timezone.
pub fn time_of(at: i64) -> String {
    chrono::DateTime::from_timestamp(at, 0)
        .map(|utc| {
            utc.with_timezone(&chrono::Local)
                .format("%H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

/// The moment a query comes back.
pub fn now() -> i64 {
    chrono::Local::now().timestamp()
}

// --- Persistence -------------------------------------------------------------

impl History {
    pub fn load() -> Self {
        let Some(path) = history_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                // Overwriting a file we failed to read would lose every
                // worktree's history over one malformed key.
                log::warn!("unreadable SQL history ({}): {e}", path.display());
                Self::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                log::warn!("reading the SQL history: {e}");
                Self::default()
            }
        }
    }

    /// The text to write. Serialised in the interface thread — it costs a
    /// millisecond — and written on a background thread by the caller: a
    /// journal is written on a gesture, never on a frame.
    pub fn serialise(&self) -> Option<String> {
        match serde_json::to_string(self) {
            Ok(json) => Some(json),
            Err(e) => {
                log::warn!("serialising the SQL history: {e}");
                None
            }
        }
    }

    /// Writes what `serialise` produced.
    pub fn write(json: &str) {
        let Some(path) = history_path() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = crate::ui::settings::write_private(&path, json) {
            log::warn!("writing the SQL history: {e}");
        }
    }
}

fn history_path() -> Option<PathBuf> {
    crate::ui::settings::config_dir().map(|dir| dir.join("sql_history.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(sql: &str, worktree: &str, at: i64) -> Entry {
        Entry {
            at,
            worktree: PathBuf::from(worktree),
            connection: "mysql:root@localhost:3306/".into(),
            label: "Acetics".into(),
            database: Some("wt_telavox_master".into()),
            sql: sql.into(),
            rows: Some(3),
            elapsed_ms: 12,
            ..Default::default()
        }
    }

    #[test]
    fn the_last_query_comes_first() {
        let mut history = History::default();
        history.record(entry("SELECT 1", "/w", 10));
        history.record(entry("SELECT 2", "/w", 20));
        assert_eq!(history.entries[0].sql, "SELECT 2");
    }

    /// Four identical rows say nothing more than one, and they push out the
    /// query from this morning that one came back for.
    #[test]
    fn running_the_same_query_again_moves_it_up_instead_of_repeating_it() {
        let mut history = History::default();
        history.record(entry("SELECT 1", "/w", 10));
        history.record(entry("SELECT 2", "/w", 20));
        let mut again = entry("select   1", "/w", 30);
        again.sql = "SELECT    1".into();
        again.elapsed_ms = 99;
        history.record(again);
        assert_eq!(history.entries.len(), 2);
        assert_eq!(history.entries[0].runs, 2);
        // The fresh result wins: what the row says is about the last run.
        assert_eq!(history.entries[0].elapsed_ms, 99);
    }

    /// The same text in another checkout is another query: that is the whole
    /// premise of a history kept per worktree.
    #[test]
    fn the_same_text_in_another_worktree_is_another_entry() {
        let mut history = History::default();
        history.record(entry("SELECT 1", "/a", 10));
        history.record(entry("SELECT 1", "/b", 20));
        assert_eq!(history.entries.len(), 2);
    }

    /// A worktree queried all afternoon must not push out the three queries of
    /// the one next door.
    #[test]
    fn the_ceiling_is_per_worktree() {
        let mut history = History::default();
        history.record(entry("SELECT 0", "/quiet", 1));
        for i in 0..PER_WORKTREE + 20 {
            history.record(entry(&format!("SELECT {i}"), "/busy", 100 + i as i64));
        }
        assert_eq!(history.count_for(Some(Path::new("/busy"))), PER_WORKTREE);
        assert_eq!(history.count_for(Some(Path::new("/quiet"))), 1);
    }

    #[test]
    fn the_reach_decides_what_is_listed() {
        let mut history = History::default();
        history.record(entry("SELECT here", "/a", 10));
        history.record(entry("SELECT there", "/b", 20));
        let here = Path::new("/a");
        let filter = Filter {
            reach: Reach::Worktree,
            worktree: Some(here),
            ..Default::default()
        };
        assert_eq!(history.matching(&filter).len(), 1);
        let filter = Filter {
            reach: Reach::All,
            worktree: Some(here),
            ..Default::default()
        };
        assert_eq!(history.matching(&filter).len(), 2);
    }

    #[test]
    fn the_search_covers_the_query_and_the_database() {
        let mut history = History::default();
        history.record(entry("SELECT * FROM users", "/a", 10));
        let mut other = entry("SELECT 1", "/a", 20);
        other.database = Some("wt_other_tenant_itcs".into());
        history.record(other);
        let find = |query| {
            history
                .matching(&Filter {
                    reach: Reach::All,
                    query,
                    ..Default::default()
                })
                .len()
        };
        assert_eq!(find("users"), 1);
        assert_eq!(find("itcs"), 1);
        assert_eq!(find(""), 2);
    }

    /// Clearing from a panel showing one worktree must not empty the others.
    #[test]
    fn clearing_only_forgets_what_is_shown() {
        let mut history = History::default();
        history.record(entry("SELECT here", "/a", 10));
        history.record(entry("SELECT there", "/b", 20));
        history.clear(&Filter {
            reach: Reach::Worktree,
            worktree: Some(Path::new("/a")),
            ..Default::default()
        });
        assert_eq!(history.entries.len(), 1);
        assert_eq!(history.entries[0].worktree, PathBuf::from("/b"));
    }

    #[test]
    fn entries_are_laid_out_under_their_day() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 8, 22).unwrap();
        let noon = |date: chrono::NaiveDate| {
            date.and_hms_opt(12, 0, 0)
                .unwrap()
                .and_local_timezone(chrono::Local)
                .unwrap()
                .timestamp()
        };
        let entries = [
            entry("SELECT 1", "/w", noon(today)),
            entry("SELECT 2", "/w", noon(today)),
            entry("SELECT 3", "/w", noon(today.pred_opt().unwrap())),
            entry(
                "SELECT 4",
                "/w",
                noon(chrono::NaiveDate::from_ymd_opt(2026, 8, 1).unwrap()),
            ),
        ];
        let borrowed: Vec<&Entry> = entries.iter().collect();
        assert_eq!(
            rows(&borrowed, today),
            vec![
                Row::Day(Day::Today),
                Row::Entry(0),
                Row::Entry(1),
                Row::Day(Day::Yesterday),
                Row::Entry(2),
                Row::Day(Day::On("2026-08-01".into())),
                Row::Entry(3),
            ]
        );
    }

    #[test]
    fn a_row_shows_the_first_line_of_its_query() {
        let entry = entry("\n  SELECT *\n  FROM users\n", "/w", 1);
        assert_eq!(entry.headline(), "SELECT *");
        assert!(entry.is_multiline());
    }

    /// A file written before a field existed must keep loading — every file
    /// already on disk, as soon as we add one.
    #[test]
    fn missing_keys_take_their_defaults() {
        let history: History =
            serde_json::from_str(r#"{"entries":[{"sql":"SELECT 1","at":5}]}"#).unwrap();
        assert_eq!(history.entries[0].runs, 1);
        assert!(history.entries[0].ok);
    }
}
