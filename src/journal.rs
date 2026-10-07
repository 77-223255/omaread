//! The journal: append-only event log, one local file.
//!
//! The journal is the source of truth. Everything else, including the reading
//! position the reader shows, is folded out of it. It lives on one machine and
//! is not meant to be shared: it is folded in place as it grows.
//!
//! Events are JSON, one per line, so a partly written last line costs at most
//! one event and never the file.

use crate::doc::Locator;
use crate::identity::BookId;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions, Permissions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Payload {
    /// A book entered the library, or was seen again at a path.
    BookSeen {
        title: Option<String>,
        authors: Vec<String>,
        path: PathBuf,
    },
    /// The reader moved to a position.
    PositionSet {
        href: String,
        block: usize,
        offset: usize,
    },
    /// Metadata was set or corrected.
    ///
    /// One event carries a patch, not the whole record: only the fields that
    /// changed are written. That keeps the journal readable and makes a later
    /// correction obvious in the log. An empty string clears a field.
    MetadataSet {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        authors: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        series: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        series_index: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tags: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rating: Option<u8>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        publisher: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        year: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
    },
    /// The book leaves the library, and nothing of it is kept.
    ///
    /// The record goes, and so does the reading position. What a scan does next
    /// starts the book over: a name read afresh, a place at the beginning.
    /// Anything less than this leaves something behind that nobody can remove
    /// afterwards, because the journal it lives in only ever grows.
    BookForgotten,
}

impl Event {
    /// Whether this event could have been written by this program.
    ///
    /// The journal is a file on disk — possibly in a folder shared between
    /// machines, possibly edited by hand — and it is the truth the library is
    /// rebuilt from, so an entry that could not have come from here is dropped
    /// rather than believed. A book is named by the hash of its contents, and it
    /// lives at an absolute path; anything else is a typo, a fragment of a line
    /// that was interrupted, or worse.
    fn is_plausible(&self) -> bool {
        let digest = self.book.strip_prefix("sha256:").unwrap_or("");
        if digest.len() != 64 || !digest.chars().all(|c| c.is_ascii_hexdigit()) {
            return false;
        }
        match &self.payload {
            Payload::BookSeen { path, .. } => path.is_absolute(),
            _ => true,
        }
    }
}

/// One line of the journal.
///
/// Only what this version writes is named here. A line written by an older
/// version may carry more — `host`, when every line still named the machine it
/// came from — and an extra key is ignored rather than rejected, so those
/// journals keep loading.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub at: DateTime<Utc>,
    pub book: String,
    #[serde(flatten)]
    pub payload: Payload,
}

/// What the events add up to. Rebuilt on startup, never stored.
#[derive(Debug, Default)]
pub struct State {
    positions: HashMap<BookId, StoredPosition>,
    books: HashMap<BookId, BookRecord>,
}

#[derive(Debug, Clone)]
struct StoredPosition {
    at: DateTime<Utc>,
    locator: Locator,
}

/// Text as it may be shown to a terminal.
///
/// A title comes from inside a book, and a terminal obeys what it is given: an
/// escape sequence in a `dc:title` can retitle a window, recolour the screen or
/// move the cursor. The terminal belongs to the reader, so what a book says is
/// shown, not obeyed. What is *recorded* is left alone: the journal holds what
/// the file said, and this is only how it is displayed.
///
/// Control characters become spaces — an escape sequence is dismantled, not
/// passed on. Unicode *format* characters (category Cf: the bidi overrides
/// such as U+202E, zero-width joiners, a byte-order mark) are invisible
/// instructions rather than text, and `char::is_control` does not cover them;
/// they are dropped while the text around them is kept, so `a\u{202E}b`
/// reads `ab` rather than `a` or `b`.
pub fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !is_format(*c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Whether a character is a Unicode format character (general category Cf):
/// an invisible or directional instruction, such as U+202E (right-to-left
/// override) or U+200E (left-to-right mark), none of which is a control
/// character. The list is the category's ranges, so a title carrying one is
/// shown as text rather than obeyed as an instruction.
fn is_format(c: char) -> bool {
    matches!(
        c as u32,
        0x00AD
            | 0x0600..=0x0605
            | 0x061C
            | 0x06DD
            | 0x070F
            | 0x0890..=0x0891
            | 0x08E2
            | 0x180E
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x2064
            | 0x2066..=0x206F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x110BD
            | 0x110CD
            | 0x13430..=0x1343F
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0001
            | 0xE0020..=0xE007F
    )
}

/// What is known about a book without opening its file.
///
/// `BookSeen` fills in what the file itself says; `MetadataSet` carries
/// corrections and everything a book file cannot express, such as series and
/// tags. A correction wins over the file.
#[derive(Debug, Clone, PartialEq)]
pub struct BookRecord {
    pub title: Option<String>,
    pub authors: Vec<String>,
    /// Every file holding this book. The same book can sit at two paths, as a
    /// copy or in two formats, and both belong to one entry rather than making a
    /// second book of it.
    pub paths: Vec<PathBuf>,
    pub series: Option<String>,
    pub series_index: Option<f32>,
    pub tags: Vec<String>,
    pub rating: Option<u8>,
    pub publisher: Option<String>,
    pub year: Option<i32>,
    pub language: Option<String>,
}

impl BookRecord {
    pub fn new(path: PathBuf) -> Self {
        Self {
            title: None,
            authors: Vec::new(),
            paths: if path.as_os_str().is_empty() {
                Vec::new()
            } else {
                vec![path]
            },
            series: None,
            series_index: None,
            tags: Vec::new(),
            rating: None,
            publisher: None,
            year: None,
            language: None,
        }
    }

    /// The file to open. The first recorded one, which is where the book was
    /// first found.
    pub fn path(&self) -> Option<&PathBuf> {
        self.paths.first()
    }

    /// Title to show, falling back to the file name so a book is never nameless.
    pub fn display_title(&self) -> String {
        clean(&self.title.clone().unwrap_or_else(|| {
            self.path()
                .and_then(|p| p.file_stem())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Untitled".into())
        }))
    }

    pub fn display_authors(&self) -> String {
        if self.authors.is_empty() {
            "-".to_string()
        } else {
            clean(&self.authors.join(", "))
        }
    }
}

impl State {
    /// The last known position of a book.
    pub fn position(&self, book: &BookId) -> Option<&Locator> {
        self.positions.get(book).map(|p| &p.locator)
    }

    pub fn book(&self, book: &BookId) -> Option<&BookRecord> {
        self.books.get(book)
    }

    /// Every book in the library, with its identity.
    pub fn books(&self) -> impl Iterator<Item = (&BookId, &BookRecord)> {
        self.books.iter()
    }

    /// Folds one event into the state.
    ///
    /// Public because a state is also built by hand: tests say what the library
    /// holds by writing the events that would have put it there.
    pub fn apply(&mut self, event: Event) {
        let book = BookId::from(event.book);
        match event.payload {
            Payload::PositionSet {
                href,
                block,
                offset,
            } => {
                let candidate = StoredPosition {
                    at: event.at,
                    locator: Locator {
                        href,
                        block,
                        offset,
                    },
                };
                match self.positions.get(&book) {
                    Some(existing) if existing.at > candidate.at => {}
                    _ => {
                        self.positions.insert(book, candidate);
                    }
                }
            }
            Payload::BookSeen {
                title,
                authors,
                path,
            } => {
                let record = self
                    .books
                    .entry(book)
                    .or_insert_with(|| BookRecord::new(path.clone()));
                if !record.paths.contains(&path) {
                    record.paths.push(path);
                }
                // A correction made later must not be undone by seeing the file
                // again, so the file's own values only fill what is still empty.
                if record.title.is_none() {
                    record.title = title;
                }
                if record.authors.is_empty() {
                    record.authors = authors;
                }
            }
            Payload::MetadataSet {
                title,
                authors,
                series,
                series_index,
                tags,
                rating,
                publisher,
                year,
                language,
            } => {
                let record = self
                    .books
                    .entry(book)
                    .or_insert_with(|| BookRecord::new(PathBuf::new()));
                // An empty string clears a field; a missing field leaves it be.
                if let Some(value) = title {
                    record.title = non_empty(value);
                }
                if let Some(value) = authors {
                    record.authors = value.into_iter().filter_map(non_empty).collect();
                }
                if let Some(value) = series {
                    record.series = non_empty(value);
                }
                if let Some(value) = series_index {
                    record.series_index = Some(value);
                }
                if let Some(value) = tags {
                    record.tags = value.into_iter().filter_map(non_empty).collect();
                }
                if let Some(value) = rating {
                    record.rating = if value == 0 { None } else { Some(value.min(5)) };
                }
                if let Some(value) = publisher {
                    record.publisher = non_empty(value);
                }
                if let Some(value) = year {
                    record.year = if value == 0 { None } else { Some(value) };
                }
                if let Some(value) = language {
                    record.language = non_empty(value);
                }
            }
            Payload::BookForgotten => {
                self.books.remove(&book);
                self.positions.remove(&book);
            }
        }
    }
}

/// Trims a value and turns an empty one into `None`, so a cleared field is
/// really cleared rather than an empty string.
fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub struct Journal {
    /// This machine's file, named after this machine. The only one written to.
    own_file: PathBuf,
    /// Guards a write against a compaction running at the same time.
    lock_file: PathBuf,
    /// The last position written, to avoid recording every keystroke.
    last_written: Option<Locator>,
}

/// A log that has grown past this is folded to the events that still matter when
/// it is next opened for writing. The fold keeps exactly the state the log held
/// (see `plan`), so it is housekeeping rather than a decision the reader has to
/// make; a log small enough to read at a glance is left alone.
const COMPACT_AFTER_BYTES: u64 = 256 * 1024;

impl Journal {
    /// Opens the journal directory, creating it when missing.
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        // What somebody has read and where they stopped is theirs, and the
        // directory may have been made by an older version or by hand:
        // permissions are tightened every time, not only when this program
        // created the directory.
        let _ = std::fs::set_permissions(dir, Permissions::from_mode(0o700));
        // Every journal in the directory, not only the one in use: a
        // `journal-<host>.jsonl` from an older version holds the same reading
        // positions and is private the same way.
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                if entry.path().extension().and_then(|e| e.to_str()) == Some("jsonl") {
                    let _ = std::fs::set_permissions(entry.path(), Permissions::from_mode(0o600));
                }
            }
        }
        let own_file = dir.join("journal.jsonl");
        // Before the first write: a folder from an older version holds one
        // `journal-<host>.jsonl` per machine, and the ones here are folded into
        // the single local file. A log that has grown long is folded too, and a
        // fold failure is not fatal: the log still replays as it is.
        let grown = std::fs::metadata(&own_file).map(|m| m.len()).unwrap_or(0);
        if (!own_file.exists() && has_journal(dir)) || grown > COMPACT_AFTER_BYTES {
            let _ = compact(dir);
        }
        Ok(Self {
            own_file,
            lock_file: dir.join(".lock"),
            last_written: None,
        })
    }

    /// Folds every journal in the directory into a state. Lines that do not
    /// parse are skipped: one damaged event, or one written by a version that
    /// has since dropped it, must not hide the rest.
    ///
    /// A line written by an older version does parse: a key this version no
    /// longer writes, such as the `host` that used to name the machine, is
    /// ignored rather than refused.
    pub fn replay(dir: &Path) -> Result<State> {
        let mut events = Vec::new();
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            // No directory yet means nothing has been read so far.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(State::default()),
            Err(err) => return Err(err).with_context(|| format!("cannot read {}", dir.display())),
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(file) = File::open(&path) else {
                continue;
            };
            for line in BufReader::new(file).lines().map_while(Result::ok) {
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(event) = serde_json::from_str::<Event>(&line)
                    && event.is_plausible() {
                        events.push(event);
                    }
            }
        }

        // Apply in time order so the newest position wins regardless of which
        // file it came from.
        events.sort_by_key(|a| a.at);
        let mut state = State::default();
        for event in events {
            state.apply(event);
        }
        Ok(state)
    }

    pub fn append(&mut self, book: &BookId, payload: Payload) -> Result<()> {
        let event = Event {
            at: Utc::now(),
            book: book.as_str().to_string(),
            payload,
        };
        let line = serde_json::to_string(&event)?;
        // Hold the lock across the open and the write, so a compaction cannot
        // rename the file out from under it and leave the event in an inode
        // nothing reads again.
        let _lock = Lock::exclusive(&self.lock_file)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.own_file)
            .with_context(|| format!("cannot append to {}", self.own_file.display()))?;
        // One write, the whole line with its newline. Two processes appending to
        // one journal — a reader and a `set` in another shell — must not
        // interleave *inside* a line, or the file the library is rebuilt from
        // grows lines nobody can read and the events they held are gone without a
        // word. `writeln!` may issue more than one write; this cannot.
        let mut bytes = line.into_bytes();
        bytes.push(b'\n');
        file.write_all(&bytes)?;
        Ok(())
    }

    /// Treats a position as already recorded. Called at startup with the
    /// position from the journal, so reopening a book without moving adds no
    /// event.
    pub fn assume_written(&mut self, locator: Option<Locator>) {
        self.last_written = locator;
    }

    /// Records a position, unless it is the one already written.
    pub fn record_position(&mut self, book: &BookId, locator: &Locator) -> Result<()> {
        if self.last_written.as_ref() == Some(locator) {
            return Ok(());
        }
        self.append(
            book,
            Payload::PositionSet {
                href: locator.href.clone(),
                block: locator.block,
                offset: locator.offset,
            },
        )?;
        self.last_written = Some(locator.clone());
        Ok(())
    }
}

/// An exclusive lock on the journal directory, held for the length of a write or
/// a compaction.
///
/// `flock` on a file of its own rather than on the journal: compaction replaces
/// the journal by rename, and a lock attached to the old file would not stop a
/// write that had already opened the path and was waiting.
struct Lock(File);

impl Lock {
    fn exclusive(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("cannot open {}", path.display()))?;
        let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if locked != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("cannot lock {}", path.display()));
        }
        Ok(Lock(file))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// What a compaction did.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Compact {
    pub before: usize,
    pub after: usize,
}

/// Whether the folder holds any journal at all, under whatever name.
fn has_journal(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries.flatten().any(|entry| {
                entry.path().extension().and_then(|e| e.to_str()) == Some("jsonl")
            })
        })
        .unwrap_or(false)
}

/// The events that reproduce `state` and nothing more.
///
/// One `book_seen` per file the book sits at, one `metadata_set` for the fields
/// a file cannot hold, one `position_set`. The history behind them — how a title
/// was corrected, where a book was before it was moved, a book that was taken
/// out and read in again — is what a log is for while it is being written, and
/// what a fold is for afterwards.
fn minimal(state: &State) -> Vec<(&BookId, Payload)> {
    let mut out = Vec::new();
    for (id, record) in state.books.iter() {
        let mut paths = record.paths.iter();
        if let Some(first) = paths.next() {
            out.push((
                id,
                Payload::BookSeen {
                    title: record.title.clone(),
                    authors: record.authors.clone(),
                    path: first.clone(),
                },
            ));
            for path in paths {
                out.push((
                    id,
                    Payload::BookSeen {
                        title: None,
                        authors: Vec::new(),
                        path: path.clone(),
                    },
                ));
            }
        }
        // An absent value is written as the empty one that clears it, so a
        // field nobody filled reads back as absent rather than as a default.
        out.push((
            id,
            Payload::MetadataSet {
                title: None,
                authors: None,
                series: Some(record.series.clone().unwrap_or_default()),
                series_index: record.series_index,
                tags: Some(record.tags.clone()),
                rating: record.rating,
                publisher: Some(record.publisher.clone().unwrap_or_default()),
                year: record.year,
                language: Some(record.language.clone().unwrap_or_default()),
            },
        ));
        if let Some(position) = state.position(id) {
            out.push((
                id,
                Payload::PositionSet {
                    href: position.href.clone(),
                    block: position.block,
                    offset: position.offset,
                },
            ));
        }
    }
    out
}

/// Folds the log to the events that still matter.
///
/// The folder is local: every `journal*.jsonl` in it is this machine's, from
/// before or after an older version split them one file per machine, and they
/// are folded into the single `journal.jsonl`. The fold keeps exactly the state
/// they held, so the library rebuilt from it is the same one.
pub fn compact(dir: &Path) -> Result<Compact> {
    let _lock = Lock::exclusive(&dir.join(".lock"))?;
    let state = Journal::replay(dir)?;
    let before = journal_lines(dir);

    let events = minimal(&state);
    let base = Utc::now();
    let mut text = String::new();
    for (index, (book, payload)) in events.iter().enumerate() {
        let event = Event {
            at: base + chrono::TimeDelta::milliseconds(index as i64),
            book: book.as_str().to_string(),
            payload: payload.clone(),
        };
        text.push_str(&serde_json::to_string(&event)?);
        text.push('\n');
    }

    // Write beside the log and rename over it: a reader sees the old file or the
    // new one, never a half-written log. The temp name does not end in `.jsonl`,
    // so a crash that leaves it behind is ignored by `replay`.
    let own = dir.join("journal.jsonl");
    let tmp = own.with_extension("tmp");
    {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("cannot write {}", tmp.display()))?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, &own).with_context(|| format!("cannot replace {}", own.display()))?;

    // The older per-machine files are folded in; only the one file is read from
    // here on.
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path != own && path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    Ok(Compact {
        before,
        after: events.len(),
    })
}

/// The number of event lines in every journal in the folder.
fn journal_lines(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| entry.path().extension().and_then(|e| e.to_str()) == Some("jsonl"))
        .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
        .map(|raw| raw.lines().filter(|line| !line.trim().is_empty()).count())
        .sum()
}

/// What the journal folder holds, for `omaread journal status`.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub files: Vec<FileStatus>,
    pub events: usize,
    pub bytes: u64,
    /// Lines that no longer matter, across every file. Only this machine's own
    /// file can be rewritten, but the count says how much of the whole folder is
    /// history rather than state.
    pub dead: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileStatus {
    pub name: String,
    pub events: usize,
    pub bytes: u64,
    pub newest: Option<DateTime<Utc>>,
}

/// Reads every journal in the folder and says how big it is and how much of it
/// still matters.
pub fn status(dir: &Path) -> Result<Status> {
    let mut status = Status {
        files: Vec::new(),
        events: 0,
        bytes: 0,
        dead: 0,
    };
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(status),
        Err(err) => return Err(err).with_context(|| format!("cannot read {}", dir.display())),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let lines: Vec<String> = raw.lines().map(str::to_string).collect();
        let newest = lines
            .iter()
            .filter_map(|line| serde_json::from_str::<Event>(line).ok())
            .map(|event| event.at)
            .max();
        status.events += lines.len();
        status.bytes += raw.len() as u64;
        status.files.push(FileStatus {
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            events: lines.len(),
            bytes: raw.len() as u64,
            newest,
        });
    }
    // What a fold would leave behind: the events the state itself needs.
    let folded = minimal(&Journal::replay(dir)?).len();
    status.dead = status.events.saturating_sub(folded);
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A book id in the shape the program writes: `sha256:` and 64 hex digits, so
    /// the tests exercise the same events a scan would write.
    fn id(seed: u8) -> BookId {
        BookId::from(format!("sha256:{}", format!("{seed:02x}").repeat(32)))
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("omaread-journal-{name}"));
        std::fs::remove_dir_all(&dir).ok();
        dir
    }

    fn locator(block: usize, offset: usize) -> Locator {
        Locator {
            href: "OEBPS/ch01.xhtml".into(),
            block,
            offset,
        }
    }

    /// The journal this machine writes, read back after the given events.
    fn written(name: &str, events: &[(&BookId, Payload)]) -> State {
        let dir = scratch(name);
        let mut journal = Journal::open(&dir).unwrap();
        for (book, payload) in events {
            journal.append(book, payload.clone()).unwrap();
        }
        let state = Journal::replay(&dir).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        state
    }

    #[test]
    fn a_recorded_position_is_written_once_and_survives_a_replay() {
        // Written once, however often the same place is recorded — in one
        // session, and in the next one that starts from the replayed state —
        // and read back as the place the reader stopped.
        let dir = scratch("roundtrip");
        let book = id(2);
        let file = || dir.join("journal.jsonl");

        let mut first = Journal::open(&dir).unwrap();
        first.record_position(&book, &locator(9, 4)).unwrap();
        first.record_position(&book, &locator(9, 4)).unwrap();

        // A second session starts from the replayed state.
        let state = Journal::replay(&dir).unwrap();
        assert_eq!(state.position(&book), Some(&locator(9, 4)));
        let mut second = Journal::open(&dir).unwrap();
        second.assume_written(state.position(&book).cloned());
        second.record_position(&book, &locator(9, 4)).unwrap();

        assert_eq!(
            std::fs::read_to_string(file()).unwrap().lines().count(),
            1
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_legacy_per_machine_file_beside_the_log_is_still_read() {
        // A folder from an older version holds a `journal-<host>.jsonl` per
        // machine. Until the next fold absorbs it, `replay` reads it beside the
        // single local log, and the newest record of a place still wins.
        let dir = scratch("newest");
        std::fs::create_dir_all(&dir).unwrap();
        let book = id(2);
        let mut journal = Journal::open(&dir).unwrap();
        journal.record_position(&book, &locator(1, 0)).unwrap();
        journal.record_position(&book, &locator(40, 5)).unwrap();

        let legacy = dir.join("journal-box.jsonl");
        std::fs::write(
            &legacy,
            "{\"at\":\"2099-01-01T00:00:00Z\",\"host\":\"box\",\"book\":\"sha256:0202020202020202020202020202020202020202020202020202020202020202\",\
             \"type\":\"position_set\",\"href\":\"OEBPS/ch01.xhtml\",\"block\":99,\"offset\":3}\n",
        )
        .unwrap();

        let state = Journal::replay(&dir).unwrap();
        assert_eq!(state.position(&book).map(|l| l.block), Some(99));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_name_cleared_by_hand_is_not_filled_in_again() {
        // `set … title=` says "use the file's own name". The reader sends the
        // file's own name only for a book the library has never seen, so opening
        // the book again must not put the `dc:title` back: a correction is not a
        // gap waiting to be filled.
        let book = id(2);
        let state = written(
            "cleared",
            &[
                (
                    &book,
                    Payload::BookSeen {
                        title: Some("一九七三年的弹子球".into()),
                        authors: vec!["村上春树".into()],
                        path: PathBuf::from("/books/a.epub"),
                    },
                ),
                (
                    &book,
                    Payload::MetadataSet {
                        title: Some(String::new()),
                        authors: None,
                        series: None,
                        series_index: None,
                        tags: None,
                        rating: None,
                        publisher: None,
                        year: None,
                        language: None,
                    },
                ),
                // Opened again: the same file, and no title of its own offered.
                (
                    &book,
                    Payload::BookSeen {
                        title: None,
                        authors: Vec::new(),
                        path: PathBuf::from("/books/a.epub"),
                    },
                ),
            ],
        );
        let record = state.book(&book).expect("the book is in the library");
        assert_eq!(record.title, None, "the clear stands");
        assert_eq!(
            record.display_title(),
            "a",
            "and what shows is the file's own name"
        );
    }

    #[test]
    fn a_forgotten_book_comes_back_as_a_new_one() {
        // Nothing is kept: what a scan adds next is a book that has no name of
        // its own recorded and no place to go back to. Forgetting takes the
        // reading position with it.
        let book = id(2);
        let dir = scratch("forget");
        let mut journal = Journal::open(&dir).unwrap();
        journal
            .append(
                &book,
                Payload::BookSeen {
                    title: Some("A Name From Inside The File".into()),
                    authors: vec!["Someone".into()],
                    path: PathBuf::from("/books/a.epub"),
                },
            )
            .unwrap();
        journal.record_position(&book, &locator(7, 12)).unwrap();

        journal.append(&book, Payload::BookForgotten).unwrap();
        let state = Journal::replay(&dir).unwrap();
        assert!(state.book(&book).is_none(), "the record is gone");
        assert_eq!(state.position(&book), None, "the place is gone too");

        // What a scan does next.
        journal
            .append(
                &book,
                Payload::BookSeen {
                    title: None,
                    authors: Vec::new(),
                    path: PathBuf::from("/books/a.epub"),
                },
            )
            .unwrap();
        let state = Journal::replay(&dir).unwrap();
        assert!(state.book(&book).is_some());
        assert_eq!(
            state.position(&book),
            None,
            "and it comes back with nothing kept"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_event_that_could_not_have_come_from_here_is_not_believed() {
        // The journal is a file on disk, possibly edited by hand. A line naming a
        // book that is not a content hash, or a book at a path that is not
        // absolute, could not have been written by this program: believing it
        // would put a book nobody has into the library, pointing wherever the line
        // said.
        let dir = scratch("implausible");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("journal-box.jsonl");
        let real = id(1);
        std::fs::write(
            &file,
            format!(
                "{{\"at\":\"2020-01-01T00:00:00Z\",\"host\":\"box\",\"book\":\"not-a-hash\",\
                 \"type\":\"book_seen\",\"title\":\"Injected\",\"authors\":[],\"path\":\"/etc/passwd\"}}\n\
                 {{\"at\":\"2020-01-01T00:00:01Z\",\"host\":\"box\",\"book\":\"{real}\",\
                 \"type\":\"book_seen\",\"title\":\"Relative\",\"authors\":[],\"path\":\"a.epub\"}}\n\
                 {{\"at\":\"2020-01-01T00:00:02Z\",\"host\":\"box\",\"book\":\"{real}\",\
                 \"type\":\"book_seen\",\"title\":\"Real\",\"authors\":[],\"path\":\"/books/a.epub\"}}\n"
            ),
        )
        .unwrap();

        let state = Journal::replay(&dir).unwrap();
        assert_eq!(state.books().count(), 1, "only the line that could be real");
        assert_eq!(
            state.book(&real).expect("the real book").display_title(),
            "Real"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_damaged_line_does_not_hide_the_others() {
        let dir = scratch("damaged");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("journal-box.jsonl");
        std::fs::write(
            &file,
            "not json at all\n\
             {\"at\":\"2020-01-01T00:00:00Z\",\"host\":\"box\",\"book\":\"sha256:0202020202020202020202020202020202020202020202020202020202020202\",\
             \"type\":\"position_set\",\"href\":\"c.xhtml\",\"block\":5,\"offset\":0}\n\
             {\"at\":\"2020-01-01T00:00:01Z\",\"host\":\"box\",\"book\":\
             \n",
        )
        .unwrap();

        let state = Journal::replay(&dir).unwrap();
        let book = id(2);
        assert_eq!(state.position(&book).map(|l| l.block), Some(5));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_event_this_version_no_longer_writes_is_skipped() {
        // A journal written before highlighting was removed, and before the
        // `file_missing` event went with it, still holds those events. The
        // payload is no longer one this reader knows, so the line fails to
        // parse and is skipped like any other unreadable line; the books
        // around it must still replay.
        let dir = scratch("old-event");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("journal-box.jsonl");
        std::fs::write(
            &file,
            "{\"at\":\"2020-01-01T00:00:00Z\",\"host\":\"box\",\"book\":\"sha256:0101010101010101010101010101010101010101010101010101010101010101\",\
             \"type\":\"book_seen\",\"title\":\"First\",\"authors\":[],\"path\":\"/books/a.epub\"}\n\
             {\"at\":\"2020-01-01T00:00:01Z\",\"host\":\"box\",\"book\":\"sha256:0101010101010101010101010101010101010101010101010101010101010101\",\
             \"type\":\"highlight_added\",\"id\":\"h-1\",\"href\":\"c.xhtml\",\
             \"slices\":[{\"block\":1,\"start\":0,\"end\":4}],\"color\":\"yellow\",\"quote\":\"word\"}\n\
             {\"at\":\"2020-01-01T00:00:02Z\",\"host\":\"box\",\"book\":\"sha256:0101010101010101010101010101010101010101010101010101010101010101\",\
             \"type\":\"file_missing\",\"path\":\"/books/a.epub\"}\n\
             {\"at\":\"2020-01-01T00:00:03Z\",\"host\":\"box\",\"book\":\"sha256:0303030303030303030303030303030303030303030303030303030303030303\",\
             \"type\":\"book_seen\",\"title\":\"Second\",\"authors\":[],\"path\":\"/books/b.epub\"}\n",
        )
        .unwrap();

        let state = Journal::replay(&dir).unwrap();
        let first = id(1);
        let second = id(3);
        assert!(state.book(&first).is_some(), "the line before is replayed");
        assert!(state.book(&second).is_some(), "the line after is replayed");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_line_naming_its_machine_still_loads() {
        // Every line used to carry the machine it came from as a `host` key.
        // The key is no longer written, but an old journal must keep loading: a
        // key this version does not know is ignored, not refused.
        let dir = scratch("old-host");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("journal-box.jsonl");
        std::fs::write(
            &file,
            "{\"at\":\"2020-01-01T00:00:00Z\",\"host\":\"box\",\"book\":\"sha256:0101010101010101010101010101010101010101010101010101010101010101\",\
             \"type\":\"book_seen\",\"title\":\"Old\",\"authors\":[],\"path\":\"/books/a.epub\"}\n",
        )
        .unwrap();

        let state = Journal::replay(&dir).unwrap();
        let book = id(1);
        assert_eq!(
            state
                .book(&book)
                .expect("the line is replayed")
                .display_title(),
            "Old"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clean_drops_format_characters_and_keeps_the_text_around_them() {
        // U+202E (right-to-left override) is a format character, not a
        // control one, so a title carrying it used to reach the terminal as
        // written and could flip the rest of the line without a single byte
        // of escape sequence. It is dropped and the text around it is kept:
        // `a\u{202E}b` reads `ab`, not `a`.
        assert_eq!(clean("abc"), "abc");
        assert_eq!(clean("a\u{202E}b"), "ab");
        assert_eq!(clean("a\u{200E}b\u{200D}c"), "abc");
        assert_eq!(clean("BOM\u{FEFF}end"), "BOMend");
        // Control characters become spaces, as they always did: an escape
        // sequence is dismantled, so a file name cannot retitle the window.
        assert_eq!(clean("a\u{85}b"), "a b");
        assert_eq!(
            clean("\x1b]0;window title\x07done"),
            " ]0;window title done"
        );
    }

    #[test]
    fn the_journal_directory_and_every_journal_in_it_are_private() {
        // The directory may have been made by an older version, by hand, or by a
        // sync tool, and a per-machine `journal-<host>.jsonl` from before may sit
        // beside the single log: both are tightened, not only the current file.
        let dir = scratch("private");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, Permissions::from_mode(0o755)).unwrap();
        // The single log exists, so opening does not fold the old file away
        // before the permissions can be checked.
        std::fs::write(dir.join("journal.jsonl"), "").unwrap();
        let legacy = dir.join("journal-box.jsonl");
        std::fs::write(&legacy, "").unwrap();
        std::fs::set_permissions(&legacy, Permissions::from_mode(0o644)).unwrap();

        Journal::open(&dir).unwrap();

        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700,
            "the directory is open to the whole machine"
        );
        assert_eq!(
            std::fs::metadata(&legacy).unwrap().permissions().mode() & 0o777,
            0o600,
            "an old journal beside the log stays readable to all"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A `metadata_set` changing just the title.
    fn meta(title: &str) -> Payload {
        Payload::MetadataSet {
            title: Some(title.to_string()),
            authors: None,
            series: None,
            series_index: None,
            tags: None,
            rating: None,
            publisher: None,
            year: None,
            language: None,
        }
    }

    /// Writes events straight to this machine's file with increasing timestamps
    /// and returns the directory. `append`'s wall clock is not ordered enough to
    /// say which event came first.
    fn journal_of(name: &str, events: &[(&BookId, Payload)]) -> PathBuf {
        let dir = scratch(name);
        Journal::open(&dir).unwrap();
        let own = dir.join("journal.jsonl");
        let mut text = String::new();
        let base = Utc::now();
        for (index, (book, payload)) in events.iter().enumerate() {
            let event = Event {
                at: base + chrono::TimeDelta::seconds(index as i64),
                book: book.as_str().to_string(),
                payload: payload.clone(),
            };
            text.push_str(&serde_json::to_string(&event).unwrap());
            text.push('\n');
        }
        std::fs::write(&own, text).unwrap();
        dir
    }

    #[test]
    fn compaction_keeps_the_state_and_drops_what_cannot_matter() {
        let a = id(1);
        let b = id(2);
        let dir = journal_of(
            "compact",
            &[
                // A was forgotten and read in again: its first life is dead.
                (
                    &a,
                    Payload::BookSeen {
                        title: Some("Old".into()),
                        authors: vec!["X".into()],
                        path: "/books/a.epub".into(),
                    },
                ),
                (&a, meta("Corrected")),
                (&a, Payload::BookForgotten),
                (
                    &a,
                    Payload::BookSeen {
                        title: Some("New".into()),
                        authors: vec!["Y".into()],
                        path: "/books/a.epub".into(),
                    },
                ),
                (
                    &a,
                    Payload::PositionSet {
                        href: "c1".into(),
                        block: 1,
                        offset: 0,
                    },
                ),
                (
                    &a,
                    Payload::PositionSet {
                        href: "c1".into(),
                        block: 9,
                        offset: 3,
                    },
                ),
                // B was never forgotten: everything it did is kept.
                (
                    &b,
                    Payload::BookSeen {
                        title: Some("B".into()),
                        authors: vec![],
                        path: "/books/b.epub".into(),
                    },
                ),
                (&b, meta("Bee")),
            ],
        );

        let before = Journal::replay(&dir).unwrap();
        let report = compact(&dir).unwrap();
        let after = Journal::replay(&dir).unwrap();

        assert_eq!(report.before, 8);
        assert_eq!(report.after, 5, "A's first life and the older position go");
        assert_eq!(before.books.len(), after.books.len());
        for (id, record) in before.books.iter() {
            let now = after.book(id).unwrap();
            assert_eq!(now.title, record.title);
            assert_eq!(now.authors, record.authors);
            assert_eq!(now.paths, record.paths);
        }
        assert_eq!(before.position(&a), after.position(&a));
        assert_eq!(before.position(&b), after.position(&b));
        // Only one position for A is left in the file.
        let raw = std::fs::read_to_string(dir.join("journal.jsonl")).unwrap();
        assert_eq!(raw.matches("position_set").count(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_fold_absorbs_a_legacy_per_machine_file() {
        // A folder from before the log became one local file holds a
        // `journal-<host>.jsonl`. A fold reads it, writes the single log, and
        // removes the old file; the state is the same either way.
        let dir = scratch("compact-legacy");
        std::fs::create_dir_all(&dir).unwrap();
        let a = id(3);
        let event = Event {
            at: Utc::now(),
            book: a.as_str().to_string(),
            payload: Payload::BookSeen {
                title: Some("Elsewhere".into()),
                authors: vec![],
                path: "/books/e.epub".into(),
            },
        };
        let legacy = dir.join("journal-box.jsonl");
        std::fs::write(
            &legacy,
            format!("{}\n", serde_json::to_string(&event).unwrap()),
        )
        .unwrap();
        let before = Journal::replay(&dir).unwrap();

        compact(&dir).unwrap();

        assert!(!legacy.exists(), "the old file was not absorbed");
        let after = Journal::replay(&dir).unwrap();
        assert_eq!(
            after.book(&a).map(|r| r.display_title()),
            before.book(&a).map(|r| r.display_title())
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
