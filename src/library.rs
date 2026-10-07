//! The library: every book, with its metadata.
//!
//! This is a read model, folded out of the journal on startup. It is not stored:
//! the journal is the truth, and a few thousand events fold in milliseconds. A
//! database would add a schema and migrations for no gain at this size; it can
//! come when a full-text index over every book calls for one.
//!
//! A book is identified by the hash of its contents, never by its path, so
//! moving or renaming a file keeps its metadata and reading position. See
//! `identity`.

use crate::epub::Book;
use crate::identity::BookId;
use crate::journal::{BookRecord, Journal, Payload, State};
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

/// One row of the library.
#[derive(Debug, Clone)]
pub struct Entry {
    pub id: BookId,
    pub record: BookRecord,
}

/// The one sentence for a reference that picked out no book.
///
/// The table, the search, the shelf and the error a command fails with all say
/// it this way, so there is one spelling of "nothing matched" to read.
pub fn no_match(needle: &str) -> String {
    format!("no book matches {needle}")
}

/// How long a prefix of an id may be before it counts as one.
///
/// Sixty-four characters of hex are nobody's idea of a name; eight is short
/// enough to recognise and long enough that no title is mistaken for one.
const MIN_PREFIX: usize = 8;

/// How much of an id is shown when a name fits several books.
///
/// Twelve characters copy in one motion and, in any library a person keeps,
/// already tell every id apart; the width only grows when two share that much.
const SHORT_PREFIX: usize = 12;

/// How the list is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    Title,
    Author,
    /// Series first, then position within it; books without a series last.
    Series,
}

impl Order {
    pub fn label(self) -> &'static str {
        match self {
            Order::Title => "title",
            Order::Author => "author",
            Order::Series => "series",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Order::Title => Order::Author,
            Order::Author => Order::Series,
            Order::Series => Order::Title,
        }
    }
}

/// The title a file name suggests.
///
/// A library kept by hand is often named better than the files inside it are: the
/// file name says which book it is, and the `dc:title` inside the EPUB says
/// whatever the person who made it felt like saying.
fn title_from_name(path: &Path) -> Option<String> {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
}

/// What a scan writes down for a book it has not seen before: what to call it,
/// and who wrote it.
///
/// The author is always the one inside the file. A folder says where a book is
/// kept, and a library is kept in folders named `books`, or by category, or by
/// whatever was convenient — none of which is a person. `--filenames` changes
/// where the title comes from and nothing else.
fn name_of(
    metadata: &crate::epub::Metadata,
    path: &Path,
    filenames: bool,
) -> (Option<String>, Vec<String>) {
    let title = match filenames {
        true => title_from_name(path).or_else(|| metadata.title.clone()),
        false => metadata.title.clone(),
    };
    (title, metadata.authors.clone())
}

/// Finds one book by its id, by a prefix of one, or by a name that matches
/// exactly one book.
///
/// An id is exact and matches one thing by construction; a prefix of one is
/// the same answer when it fits one book, and no answer at all when it fits
/// several. A title is what a person has in front of them, so it matches
/// loosely — but a loose match that fits several books is not an answer
/// either, and the caller is told which ones it could have meant rather than
/// being handed the first.
pub fn resolve(state: &State, needle: &str) -> Result<BookId> {
    let wanted = needle.trim();
    let all = entries(state);

    // An id is exact, and matches one thing by construction.
    if let Some(entry) = all.iter().find(|entry| entry.id.as_str() == wanted) {
        return Ok(entry.id.clone());
    }

    // So is a long enough prefix of one — but only while one book answers to
    // it. Two books sharing the prefix are named apart below rather than
    // having the first of them picked.
    if wanted.chars().count() >= MIN_PREFIX {
        let prefixed: Vec<&Entry> = all
            .iter()
            .filter(|entry| entry.id.as_str().starts_with(wanted))
            .collect();
        match prefixed.len() {
            1 => return Ok(prefixed[0].id.clone()),
            0 => {}
            _ => bail!(
                "{} books have ids starting with {wanted}; name one by id:\n{}",
                prefixed.len(),
                candidates(&prefixed)
            ),
        }
    }

    // Everything else goes through the same matcher `list --filter` uses, so
    // the command line and the library view agree about what a word finds.
    let matches = filter(&all, wanted);
    match matches.len() {
        0 => bail!("{}", no_match(needle)),
        1 => Ok(matches[0].id.clone()),
        _ => {
            let listed: Vec<&Entry> = matches.iter().collect();
            // A one-letter needle matches most of a library, and a wall of names
            // is not an answer to anything. Ten says what shape the problem is.
            bail!(
                "{} books match {wanted}; name one by id:\n{}",
                matches.len(),
                candidates(&listed)
            )
        }
    }
}

/// The books a reference could have meant, titled and named by a short prefix
/// of each id — long enough to tell them apart, short enough to copy.
///
/// Ten at the most: a wall of names is not an answer to anything, and the rest
/// are counted rather than listed.
fn candidates(matches: &[&Entry]) -> String {
    let shown = &matches[..matches.len().min(10)];
    let ids: Vec<&BookId> = shown.iter().map(|entry| &entry.id).collect();
    let prefixes = distinguishing_prefixes(&ids);
    let mut lines: Vec<String> = shown
        .iter()
        .zip(&prefixes)
        .map(|(entry, prefix)| format!("{}  {prefix}", entry.record.display_title()))
        .collect();
    let more = matches.len().saturating_sub(shown.len());
    if more > 0 {
        lines.push(format!("… and {more} more"));
    }
    lines.join("\n")
}

/// The shortest prefix of each id, no shorter than [`SHORT_PREFIX`], that
/// still tells every one of them apart.
///
/// The ids are one length in practice, so this settles quickly; the width
/// grows only when two candidates share their first twelve characters.
fn distinguishing_prefixes(ids: &[&BookId]) -> Vec<String> {
    let longest = ids
        .iter()
        .map(|id| id.as_str().chars().count())
        .max()
        .unwrap_or(SHORT_PREFIX);
    let mut width = SHORT_PREFIX.min(longest).max(1);
    loop {
        let prefixes: Vec<String> = ids
            .iter()
            .map(|id| id.as_str().chars().take(width).collect())
            .collect();
        let apart = prefixes
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            == prefixes.len();
        if apart || width >= longest {
            return prefixes;
        }
        width += 1;
    }
}

/// Builds the library from a replayed journal.
pub fn entries(state: &State) -> Vec<Entry> {
    state
        .books()
        .map(|(id, record)| Entry {
            id: id.clone(),
            record: record.clone(),
        })
        .collect()
}

/// Sorts a list in place.
pub fn sort(entries: &mut [Entry], order: Order) {
    match order {
        Order::Title => entries.sort_by_key(|e| sortable(&e.record.display_title())),
        Order::Author => entries.sort_by(|a, b| {
            // A book without an author belongs at the end, not in front of
            // everything: its placeholder would otherwise sort before the letters.
            let key = |e: &Entry| {
                e.record
                    .authors
                    .first()
                    .filter(|name| !name.trim().is_empty())
                    .map(|name| sortable(name))
            };
            match (key(a), key(b)) {
                (Some(x), Some(y)) => x.cmp(&y).then_with(|| {
                    sortable(&a.record.display_title()).cmp(&sortable(&b.record.display_title()))
                }),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => {
                    sortable(&a.record.display_title()).cmp(&sortable(&b.record.display_title()))
                }
            }
        }),
        Order::Series => entries.sort_by(|a, b| {
            // Books outside a series come last rather than clumping under an
            // empty heading.
            let key = |e: &Entry| e.record.series.as_ref().map(|s| sortable(s));
            match (key(a), key(b)) {
                (Some(x), Some(y)) => x.cmp(&y).then_with(|| {
                    a.record
                        .series_index
                        .unwrap_or(f32::MAX)
                        .total_cmp(&b.record.series_index.unwrap_or(f32::MAX))
                }),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => {
                    sortable(&a.record.display_title()).cmp(&sortable(&b.record.display_title()))
                }
            }
        }),
    }
}

/// Sort key: lowercase, and a leading article dropped so "The Hobbit" files
/// under H.
fn sortable(text: &str) -> String {
    let lower = text.trim().to_lowercase();
    for article in ["the ", "a ", "an ", "der ", "die ", "das ", "ein ", "eine "] {
        if let Some(rest) = lower.strip_prefix(article) {
            return rest.to_string();
        }
    }
    lower
}

/// The books a word picks out.
///
/// This is the one matcher in the program: `list --filter`, `show`,
/// `omaread BOOK` and `forget` all come through it, so a word that
/// finds a book in one of them finds it in every one of them. It looks at the
/// title, the authors, the series and the tags, and puts the books whose fields
/// *start* with the word first — which is what a person means when they type the
/// first letters of a title.
pub fn filter(entries: &[Entry], needle: &str) -> Vec<Entry> {
    if needle.trim().is_empty() {
        return entries.to_vec();
    }
    let needle = needle.trim().to_lowercase();

    let at_word_start: Vec<Entry> = entries
        .iter()
        .filter(|entry| {
            fields_of(entry)
                .iter()
                .any(|text| starts_a_word(text, &needle))
        })
        .cloned()
        .collect();
    if !at_word_start.is_empty() {
        return at_word_start;
    }

    entries
        .iter()
        .filter(|entry| fields_of(entry).iter().any(|text| text.contains(&needle)))
        .cloned()
        .collect()
}

/// The fields a filter looks at, lowercased.
fn fields_of(entry: &Entry) -> [String; 4] {
    let record = &entry.record;
    [
        record.display_title().to_lowercase(),
        record.display_authors().to_lowercase(),
        record.series.clone().unwrap_or_default().to_lowercase(),
        record.tags.join(" ").to_lowercase(),
    ]
}

/// True when the needle appears at the start of a word.
fn starts_a_word(text: &str, needle: &str) -> bool {
    let mut from = 0;
    while let Some(found) = text[from..].find(needle) {
        let at = from + found;
        let before = text[..at].chars().next_back();
        if before.is_none_or(|c| !c.is_alphanumeric()) {
            return true;
        }
        from = at + needle.len();
    }
    false
}

/// What a scan did, for the report afterwards.
#[derive(Debug, Default)]
pub struct ScanReport {
    pub seen: usize,
    pub added: usize,
    pub moved: usize,
    pub unreadable: Vec<(PathBuf, String)>,
}

/// Walks a directory and records every book it can read.
///
/// A file already known by its hash is not added again; if it turned up
/// elsewhere, only its path is corrected. Metadata from the book itself is
/// written once, on first sight, so a later correction of yours is never
/// overwritten by a re-scan.
pub fn scan(
    dir: &Path,
    journal: &mut Journal,
    state: &State,
    filenames: bool,
    report_progress: &mut dyn FnMut(&Path),
) -> Result<ScanReport> {
    let mut report = ScanReport::default();

    for path in collect_books(dir) {
        report_progress(&path);
        report.seen += 1;

        let id = match BookId::of_file(&path) {
            Ok(id) => id,
            Err(err) => {
                report.unreadable.push((path, format!("{err:#}")));
                continue;
            }
        };

        let known = state.book(&id);
        let here = path.canonicalize().unwrap_or_else(|_| path.clone());
        if let Some(record) = known {
            if record.paths.contains(&here) {
                // Already here, and a scan never overwrites what it records:
                // that is the rule that keeps a correction of yours from being
                // undone. To have a book read again, forget it first.
                continue;
            }
            // Same contents at a path we did not know: either the file moved or
            // there is a second copy. Both mean one more file for this book.
            journal.append(
                &id,
                Payload::BookSeen {
                    title: record.title.clone(),
                    authors: record.authors.clone(),
                    path: here,
                },
            )?;
            report.moved += 1;
            continue;
        }

        // New book: read what the file says about itself.
        let mut book = match Book::open(&path) {
            Ok(book) => book,
            Err(err) => {
                report.unreadable.push((path, format!("{err:#}")));
                continue;
            }
        };
        let metadata = std::mem::take(&mut book.metadata);
        let (title, authors) = name_of(&metadata, &path, filenames);
        journal.append(
            &id,
            Payload::BookSeen {
                title,
                authors,
                path: here,
            },
        )?;
        // Fields that BookSeen does not carry.
        if metadata.language.is_some() {
            journal.append(
                &id,
                Payload::MetadataSet {
                    title: None,
                    authors: None,
                    series: None,
                    series_index: None,
                    tags: None,
                    rating: None,
                    publisher: None,
                    year: None,
                    language: metadata.language.clone(),
                },
            )?;
        }
        report.added += 1;
    }

    Ok(report)
}

/// Every readable book file below a directory, in a stable order.
fn collect_books(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    walk(dir, &mut found, 0);
    found.sort();
    found
}

/// Depth limit, so a symlink loop cannot run away with the scan.
const MAX_DEPTH: usize = 12;

fn walk(dir: &Path, found: &mut Vec<PathBuf>, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Hidden directories hold caches and version control, not books.
        if name.starts_with('.') {
            continue;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => walk(&path, found, depth + 1),
            Ok(kind) if kind.is_file()
                && is_book(&path) => {
                    found.push(path);
                }
            _ => {}
        }
    }
}

fn is_book(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("epub")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{Event, Payload, State};
    use chrono::Utc;

    /// A book, as a scan would have written it.
    fn seen(id: &str, title: &str, author: &str) -> (crate::identity::BookId, Event) {
        let book = crate::identity::BookId::from(id.to_string());
        let event = Event {
            at: Utc::now(),
            book: book.to_string(),
            payload: Payload::BookSeen {
                title: Some(title.into()),
                authors: vec![author.into()],
                path: std::path::PathBuf::from(format!("/books/{title}.epub")),
            },
        };
        (book, event)
    }

    #[test]
    fn naming_a_book_answers_with_one_book_or_says_which() {
        let mut state = State::default();
        let (adams, event) = seen("sha256:adams", "The Hitchhiker's Guide", "Douglas Adams");
        state.apply(event);
        let (tolkien, event) = seen(
            "sha256:tolkien",
            "The Lord of the Rings",
            "J. R. R. Tolkien",
        );
        state.apply(event);

        // An id is exact, whatever else the words might have matched.
        assert_eq!(resolve(&state, "sha256:tolkien").unwrap(), tolkien);
        // A word that fits one book picks it.
        assert_eq!(resolve(&state, "hitchhiker").unwrap(), adams);
        // An author names a book as well as a title does.
        assert_eq!(resolve(&state, "tolkien").unwrap(), tolkien);
        // A word that fits several is not an answer.
        let err = resolve(&state, "the").unwrap_err().to_string();
        assert!(err.contains("2 books match"), "{err}");
        assert!(err.contains("name one by id"), "{err}");
        // The word is shown as it was typed, not debugged into quotes.
        assert!(err.contains("match the;"), "{err}");
    }

    #[test]
    fn an_id_prefix_names_the_book_when_only_one_answers() {
        // 64 characters of hex are what nobody wants to copy: the start of one
        // is enough when only one book answers to it.
        let first = "sha256:abcdef111111111111111111111111111111111111111111111111111111111";
        let mut state = State::default();
        let (_, event) = seen(first, "First", "Someone");
        state.apply(event);
        let id = resolve(&state, "sha256:abcdef111111111111111111111111111111111").unwrap();
        assert_eq!(id.as_str(), first);

        // Below the minimum, an id-shaped word is nothing but a word: it goes
        // through the matcher like any title, and answers when nothing fits.
        let err = resolve(&state, "sha256").unwrap_err().to_string();
        assert!(err.contains("no book matches sha256"), "{err}");

        // A prefix two books share is not an answer — each is named by a
        // prefix that tells them apart, and one more character is an answer.
        let mut state = State::default();
        for (hex, title) in [("abcdef1111", "First"), ("abcdef2222", "Second")] {
            let (_, event) = seen(&format!("sha256:{hex}"), title, "Someone");
            state.apply(event);
        }
        let err = resolve(&state, "sha256:abcdef").unwrap_err().to_string();
        assert!(
            err.contains("2 books have ids starting with sha256:abcdef"),
            "{err}"
        );
        assert!(err.contains("name one by id"), "{err}");
        assert!(err.contains("sha256:abcdef1"), "{err}");
        assert!(err.contains("sha256:abcdef2"), "{err}");
        assert_eq!(
            resolve(&state, "sha256:abcdef1").unwrap().as_str(),
            "sha256:abcdef1111"
        );
    }

    #[test]
    fn the_candidates_are_shown_by_short_prefixes_and_counted() {
        // Twelve books, all of them fitting the same word: the answer counts
        // them all, lists ten of them, and names each one by a prefix short
        // enough to copy rather than a whole 71-character id.
        let mut state = State::default();
        for n in 0..12 {
            let id = format!("sha256:abc{n}{}", "d".repeat(60));
            let (_, event) = seen(&id, &format!("The Book {n}"), "Someone");
            state.apply(event);
        }
        let err = resolve(&state, "the").unwrap_err().to_string();
        assert!(err.contains("12 books match"), "{err}");
        assert_eq!(err.matches("sha256:").count(), 10, "{err}");
        assert!(err.contains("and 2 more"), "{err}");
        for line in err.lines().filter(|line| line.contains("sha256:")) {
            let prefix = line.split_whitespace().last().unwrap();
            assert!(prefix.starts_with("sha256:"), "{line}");
            assert!(prefix.chars().count() >= 12, "{line}");
            assert!(prefix.chars().count() < 71, "full id copied: {line}");
        }
    }

    fn entry(title: &str, author: &str, series: Option<&str>, index: Option<f32>) -> Entry {
        let mut record = BookRecord::new(PathBuf::from("/tmp/x.epub"));
        record.title = Some(title.into());
        record.authors = vec![author.into()];
        record.series = series.map(String::from);
        record.series_index = index;
        Entry {
            id: BookId::from(format!("sha256:{title}")),
            record,
        }
    }

    #[test]
    fn sorts_by_title_ignoring_a_leading_article() {
        assert_eq!(sortable("The Hobbit"), "hobbit");
        assert_eq!(sortable("Die Verwandlung"), "verwandlung");
        assert_eq!(sortable("Anathem"), "anathem");

        let mut list = vec![
            entry("Zero to Sold", "Bechtel", None, None),
            entry("The Hobbit", "Tolkien", None, None),
        ];
        sort(&mut list, Order::Title);
        assert_eq!(list[0].record.title.as_deref(), Some("The Hobbit"));
    }

    #[test]
    fn sorting_puts_series_in_order_and_leaves_what_is_missing_last() {
        // A series is read in the order the books come in, position and all.
        let mut list = vec![
            entry("Third", "A", Some("Saga"), Some(3.0)),
            entry("Interlude", "A", Some("Saga"), Some(1.5)),
            entry("First", "A", Some("Saga"), Some(1.0)),
        ];
        sort(&mut list, Order::Series);
        let titles: Vec<_> = list
            .iter()
            .map(|e| e.record.title.clone().unwrap())
            .collect();
        assert_eq!(titles, ["First", "Interlude", "Third"]);

        // And a book without the field being sorted on comes last, in the
        // author order as much as in the series one.
        let mut list = vec![
            entry("Loose", "A", None, None),
            entry("In a series", "A", Some("Saga"), Some(1.0)),
        ];
        sort(&mut list, Order::Series);
        assert_eq!(list[0].record.series.as_deref(), Some("Saga"));

        let mut nameless = entry("Zzz Anonymous", "", None, None);
        nameless.record.authors.clear();
        let mut list = vec![nameless, entry("Anathem", "Stephenson", None, None)];
        sort(&mut list, Order::Author);
        assert_eq!(
            list[0].record.authors.first().map(String::as_str),
            Some("Stephenson")
        );
    }

    #[test]
    fn a_filter_matches_anywhere_in_any_field() {
        let mut tagged = entry("Anathem", "Stephenson", None, None);
        tagged.record.tags = vec!["science fiction".into()];
        let list = vec![
            entry("The Hobbit", "Tolkien", Some("Middle-earth"), Some(1.0)),
            tagged,
        ];
        assert_eq!(filter(&list, "hobbit").len(), 1);
        assert_eq!(filter(&list, "tolkien").len(), 1);
        assert_eq!(filter(&list, "middle").len(), 1);
        assert_eq!(filter(&list, "science").len(), 1);
        assert_eq!(filter(&list, "").len(), 2);
        assert_eq!(filter(&list, "nothing here").len(), 0);

        // A match that starts a word beats one hiding inside another word,
        // and a word nothing starts with still finds what it hides inside.
        let list = vec![
            entry("Practical Fraud Prevention", "Saporta", None, None),
            entry("Understanding Eventsourcing", "Dilger", None, None),
        ];
        let found = filter(&list, "event");
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].record.title.as_deref(),
            Some("Understanding Eventsourcing")
        );
        let list = vec![entry("Understanding Eventsourcing", "Dilger", None, None)];
        assert_eq!(filter(&list, "sourcing").len(), 1);
    }

    #[test]
    fn a_book_can_sit_in_several_files() {
        let mut record = BookRecord::new(PathBuf::from("/a/book.epub"));
        record.paths.push(PathBuf::from("/b/book.epub"));
        // The first one recorded is the one to open.
        assert_eq!(record.path(), Some(&PathBuf::from("/a/book.epub")));
        assert_eq!(record.paths.len(), 2);
    }

    #[test]
    fn the_author_comes_from_the_file_whatever_the_name_comes_from() {
        let metadata = crate::epub::Metadata {
            title: Some("Norwegian Wood".into()),
            authors: vec!["Haruki Murakami".into()],
            language: None,
            identifier: None,
        };
        let path = Path::new("/books/村上春树/挪威的森林.epub");

        let (title, authors) = name_of(&metadata, path, true);
        assert_eq!(
            title.as_deref(),
            Some("挪威的森林"),
            "the file name names it"
        );
        assert_eq!(
            authors,
            vec!["Haruki Murakami".to_string()],
            "not the folder's own name"
        );

        let (title, authors) = name_of(&metadata, path, false);
        assert_eq!(
            title.as_deref(),
            Some("Norwegian Wood"),
            "the file names itself"
        );
        assert_eq!(authors, vec!["Haruki Murakami".to_string()]);
    }

    #[test]
    fn only_epub_files_count_as_books() {
        assert!(is_book(Path::new("a.epub")));
        assert!(is_book(Path::new("a.EPUB")));
        assert!(!is_book(Path::new("a.pdf")));
        assert!(!is_book(Path::new("a.mobi")));
        assert!(!is_book(Path::new("noextension")));
    }
}
