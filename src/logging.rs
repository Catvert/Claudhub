//! Logging.
//!
//! `CLAUDHUB_LOG` follows `env_logger` syntax (`CLAUDHUB_LOG=debug`,
//! `CLAUDHUB_LOG=claudhub::git=trace`). By default only warnings surface: the
//! graphics dependencies are chatty at info level, and a drowned console
//! serves nobody.
//!
//! Everything printed is **also kept in memory**, in a ring buffer the
//! settings' "Logs" page reads. A graphical application has no console under
//! its window: without that buffer, finding out why something failed means
//! relaunching from a terminal, which is asking the user to reproduce the
//! problem before being allowed to look at it. The remote server's records
//! land there too — the client pumps its stderr into ours, under the
//! `claudhub_server` target.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// How many records are kept.
///
/// A ring and not a growing list: a session lasts a day, a chatty target writes
/// a line per frame, and a log nobody has asked to see must not be able to take
/// the memory of the review it is there to explain.
const CAPACITY: usize = 2000;

/// One record, as the page shows it.
///
/// The pieces stay apart rather than being formatted at once: the level is what
/// colours the row and what the filter reads, and a line already assembled
/// would have to be parsed back to get them.
#[derive(Debug, Clone)]
pub struct Entry {
    pub at: chrono::DateTime<chrono::Local>,
    pub level: log::Level,
    /// The module that wrote it — `claudhub_server` for what comes back from
    /// the remote server.
    pub target: String,
    pub message: String,
}

fn buffer() -> &'static Mutex<VecDeque<Entry>> {
    static BUFFER: OnceLock<Mutex<VecDeque<Entry>>> = OnceLock::new();
    BUFFER.get_or_init(|| Mutex::new(VecDeque::with_capacity(CAPACITY)))
}

/// How many records have been written since the start.
///
/// A counter and not the buffer's length: the ring stops growing, and a view
/// that caches its copy needs something that says "something has happened".
/// Clearing bumps it too, for the same reason.
static WRITTEN: AtomicU64 = AtomicU64::new(0);

pub fn written() -> u64 {
    WRITTEN.load(Ordering::Relaxed)
}

/// The records kept, oldest first.
pub fn records() -> Vec<Entry> {
    match buffer().lock() {
        Ok(buffer) => buffer.iter().cloned().collect(),
        Err(_) => Vec::new(),
    }
}

pub fn clear() {
    if let Ok(mut buffer) = buffer().lock() {
        buffer.clear();
    }
    WRITTEN.fetch_add(1, Ordering::Relaxed);
}

/// A duration, as a journal line says it.
///
/// Milliseconds up to a second, then seconds with one decimal: `1450 ms` is a
/// number one has to divide before knowing whether it is a problem, where
/// `1.4 s` is read.
pub fn ms(elapsed: std::time::Duration) -> String {
    if elapsed < std::time::Duration::from_secs(1) {
        format!("{} ms", elapsed.as_millis())
    } else {
        format!("{:.1} s", elapsed.as_secs_f32())
    }
}

/// Writes to stderr, and keeps a copy.
///
/// A wrapper around `env_logger` rather than its `format` hook: the formatter
/// is handed a byte sink and is called for what will be **printed**, where the
/// page needs the level and the target as they are — one to colour the row, the
/// other to say where it came from.
struct Tee {
    inner: env_logger::Logger,
}

impl log::Log for Tee {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        self.inner.enabled(metadata)
    }

    fn log(&self, record: &log::Record) {
        // `log::set_max_level` only bounds by level: it is `enabled` that
        // applies the per-target directives of `CLAUDHUB_LOG`. Skipping it here
        // would keep in memory what the console was told not to print.
        if !self.inner.enabled(record.metadata()) {
            return;
        }
        self.inner.log(record);
        let entry = Entry {
            at: chrono::Local::now(),
            level: record.level(),
            target: record.target().to_string(),
            message: record.args().to_string(),
        };
        if let Ok(mut buffer) = buffer().lock() {
            if buffer.len() == CAPACITY {
                buffer.pop_front();
            }
            buffer.push_back(entry);
        }
        WRITTEN.fetch_add(1, Ordering::Relaxed);
    }

    fn flush(&self) {
        self.inner.flush();
    }
}

pub fn init() {
    let logger = env_logger::Builder::from_env(
        env_logger::Env::new()
            .filter_or("CLAUDHUB_LOG", "warn,claudhub=info")
            .write_style("CLAUDHUB_LOG_STYLE"),
    )
    .format_timestamp_millis()
    // `build` and not `init`: the logger installed is ours, which holds this
    // one and copies what goes through it.
    .build();
    let level = logger.filter();
    if log::set_boxed_logger(Box::new(Tee { inner: logger })).is_ok() {
        log::set_max_level(level);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Words that are French and are not English. Deliberately short, and
    /// function words only: a list of nouns would catch a path or a program
    /// name sooner or later.
    const FRENCH: &[&str] = &[
        "le", "la", "les", "un", "une", "des", "du", "dans", "pour", "qui", "que", "avec", "sans",
        "est", "sont", "aucun", "cette", "ces", "depuis", "vers", "sur",
    ];

    /// The journal is written in English, and this is what keeps it that way.
    ///
    /// The rule — the core speaks English, the documentation French — had no
    /// test, unlike the i18n catalogues which have one. It went unnoticed for
    /// as long as those lines only reached a console nobody opened; the "Logs"
    /// page shows them raw, so a stray French line is now a user-visible
    /// inconsistency next to twenty English ones.
    ///
    /// Only `log::` literals, and only those on the macro's own line: this
    /// looks at the text, not at the syntax, and widening it to every string
    /// in the tree would catch test fixtures and the accented paths they are
    /// full of on purpose.
    #[test]
    fn nothing_speaks_french_in_the_journal() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut caught = Vec::new();
        for file in rust_files(&root) {
            let text = std::fs::read_to_string(&file).expect("a source file");
            for (no, line) in text.lines().enumerate() {
                if !line.contains("log::") {
                    continue;
                }
                for word in quoted(line)
                    .split(|c: char| !c.is_alphabetic() && c != '\u{e9}' && c != '\u{e8}')
                {
                    let lower = word.to_lowercase();
                    if !word.is_ascii() || FRENCH.contains(&lower.as_str()) {
                        caught.push(format!("{}:{}: {}", file.display(), no + 1, line.trim()));
                        break;
                    }
                }
            }
        }
        assert!(
            caught.is_empty(),
            "French in the journal:\n{}",
            caught.join("\n")
        );
    }

    /// The `"…"` pieces of a line, concatenated. Escapes are not handled: a
    /// literal with a quote in it is not what this is looking for.
    fn quoted(line: &str) -> String {
        line.split('"')
            .skip(1)
            .step_by(2)
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn rust_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut files = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return files;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(rust_files(&path));
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push(path);
            }
        }
        files
    }

    #[test]
    fn a_duration_is_written_the_way_it_is_read() {
        // Milliseconds while they stay a number one can hold, then seconds:
        // `1450 ms` has to be divided before one knows whether it is a problem.
        assert_eq!(ms(std::time::Duration::from_millis(0)), "0 ms");
        assert_eq!(ms(std::time::Duration::from_millis(999)), "999 ms");
        assert_eq!(ms(std::time::Duration::from_millis(1000)), "1.0 s");
        assert_eq!(ms(std::time::Duration::from_millis(1450)), "1.5 s");
    }
}
