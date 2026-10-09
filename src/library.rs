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
use crate::i18n;
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
/// it this way, so there is one spelling of "nothing matched" to read — through
/// the one lookup, so shelf and command line say it in the same language.
pub fn no_match(needle: &str) -> String {
    i18n::fill("no book matches {}", &[&needle])
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

/// One author's shelf: the name as the books record it, and the books it names.
///
/// The books are positions into the entries the shelf already holds, so a
/// grouping copies names and indices rather than cloning the books behind them.
#[derive(Debug, Clone)]
pub struct Author {
    /// `None` for the books that name nobody — one group for all of them,
    /// parked at the end where no name sorts.
    pub name: Option<String>,
    pub books: Vec<usize>,
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
        1 => Ok(all[matches[0]].id.clone()),
        _ => {
            let listed: Vec<&Entry> = matches.iter().map(|&index| &all[index]).collect();
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

/// Sorts a whole library in place: first author, then title, and the books
/// that name nobody last.
///
/// The one order anything that prints every book uses — the shelf browses in
/// it and `omaread list` prints in it, so a list read on the terminal and a
/// shelf opened on it agree about what comes first.
pub fn sort_books(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
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
    })
}

/// Every author on the shelf, alphabetical, with the nameless group last.
///
/// A book is filed under its *first* author and no other: a book written by
/// several would otherwise sit in every list at once, and the first name is
/// the one that names it on the cover. The group's books arrive sorted by
/// title, so one shelf level needs no second ordering of its own.
pub fn authors(entries: &[Entry]) -> Vec<Author> {
    let mut groups: Vec<Author> = Vec::new();
    // The name as written is the key, so one spelling of a name is one row and
    // the mess of spellings stays visible for `omaread authors` to fix.
    let mut by_name: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut nameless: Option<usize> = None;

    for (index, entry) in entries.iter().enumerate() {
        let first = entry
            .record
            .authors
            .first()
            .map(String::as_str)
            .filter(|name| !name.trim().is_empty());
        let group = match first {
            Some(name) => match by_name.get(name) {
                Some(&group) => group,
                None => {
                    groups.push(Author {
                        name: Some(name.to_string()),
                        books: Vec::new(),
                    });
                    by_name.insert(name, groups.len() - 1);
                    groups.len() - 1
                }
            },
            // One group for every book with no author, however many there are.
            None => *nameless.get_or_insert_with(|| {
                groups.push(Author {
                    name: None,
                    books: Vec::new(),
                });
                groups.len() - 1
            }),
        };
        groups[group].books.push(index);
    }

    groups.sort_by(|a, b| match (&a.name, &b.name) {
        (Some(x), Some(y)) => sortable(x).cmp(&sortable(y)),
        // The nameless group has no name to sort with, so it trails the rest
        // rather than borrowing a placeholder that would file it under a letter.
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    for group in &mut groups {
        // Cached: the lowercase key would otherwise be built again for every
        // comparison, once per pair in a large library.
        group
            .books
            .sort_by_cached_key(|&index| sortable(&entries[index].record.display_title()));
    }
    groups
}

/// The positions in `authors` of the names a word picks out, in list order.
///
/// The same folding the book filter does — trimmed, lowercased, a name the
/// word starts ahead of one it only hides inside — so a word that finds an
/// author here finds the same author's books in `filter`. The nameless group
/// has no name to look in, and a word never picks it.
pub fn filter_authors(authors: &[Author], needle: &str) -> Vec<usize> {
    if needle.trim().is_empty() {
        return (0..authors.len()).collect();
    }
    let needle = needle.trim().to_lowercase();
    // Lowercased once per name rather than once per pass over them.
    let names: Vec<Option<String>> = authors
        .iter()
        .map(|author| author.name.as_ref().map(|name| name.to_lowercase()))
        .collect();

    let at_word_start: Vec<usize> = names
        .iter()
        .enumerate()
        .filter(|(_, name)| name.as_deref().is_some_and(|name| starts_a_word(name, &needle)))
        .map(|(index, _)| index)
        .collect();
    if !at_word_start.is_empty() {
        return at_word_start;
    }

    names
        .iter()
        .enumerate()
        .filter(|(_, name)| name.as_deref().is_some_and(|name| name.contains(&needle)))
        .map(|(index, _)| index)
        .collect()
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

/// The positions in `entries` of the books a word picks out, in list order.
///
/// This is the one matcher in the program: `list --filter`, `show`,
/// `omaread BOOK` and `forget` all come through it, so a word that
/// finds a book in one of them finds it in every one of them. It looks at the
/// title, the authors, the series and the tags, and puts the books whose fields
/// *start* with the word first — which is what a person means when they type the
/// first letters of a title.
///
/// Answers with positions rather than copies of the books: every caller
/// already holds the list — a shelf re-filtering while a key is held down, a
/// command printing a table — and copying the matches out would clone the
/// library per call to learn which of its own rows were picked.
pub fn filter(entries: &[Entry], needle: &str) -> Vec<usize> {
    if needle.trim().is_empty() {
        return (0..entries.len()).collect();
    }
    let needle = needle.trim().to_lowercase();
    // Lowercased once per book, not once per pass: the word-start pass and the
    // fallback look at the same four fields, and a book matched nowhere built
    // them twice for nothing.
    let fields: Vec<[String; 4]> = entries.iter().map(fields_of).collect();

    let at_word_start: Vec<usize> = fields
        .iter()
        .enumerate()
        .filter(|(_, fields)| fields.iter().any(|text| starts_a_word(text, &needle)))
        .map(|(index, _)| index)
        .collect();
    if !at_word_start.is_empty() {
        return at_word_start;
    }

    fields
        .iter()
        .enumerate()
        .filter(|(_, fields)| fields.iter().any(|text| text.contains(&needle)))
        .map(|(index, _)| index)
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
    use crate::journal::State;
    use crate::journal::tests::seen;

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
        // The sentence is looked up rather than spelled out, so the assertion
        // reads the same on a machine set to any language.
        let err = resolve(&state, "sha256").unwrap_err().to_string();
        assert!(
            err.contains(&i18n::fill("no book matches {}", &[&"sha256"])),
            "{err}"
        );

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

    fn entry(title: &str, authors: &[&str], series: Option<&str>, index: Option<f32>) -> Entry {
        let mut record = BookRecord::new(PathBuf::from("/tmp/x.epub"));
        record.title = Some(title.into());
        record.authors = authors.iter().map(|name| (*name).to_string()).collect();
        record.series = series.map(String::from);
        record.series_index = index;
        Entry {
            id: BookId::from(format!("sha256:{title}")),
            record,
        }
    }

    #[test]
    fn authors_are_grouped_alphabetically_with_the_nameless_one_last() {
        let list = vec![
            entry("Zebra", &["Zena"], None, None),
            entry("Apple", &["Adam"], None, None),
            entry("Mango", &["Mid"], None, None),
            entry("Anonymous", &[], None, None),
        ];
        let groups = authors(&list);
        let names: Vec<Option<&str>> = groups.iter().map(|group| group.name.as_deref()).collect();
        assert_eq!(
            names,
            vec![Some("Adam"), Some("Mid"), Some("Zena"), None],
            "alphabetically, and nobody at the end"
        );
        // Every authorless book lands in the one group, not one each.
        assert_eq!(groups[3].books, vec![3]);
    }

    #[test]
    fn a_book_is_filed_under_its_first_author_only() {
        // A book by two people is one book: listing it under both would show it
        // twice on a shelf that only ever holds it once.
        let list = vec![
            entry("Good Omens", &["Terry Pratchett", "Neil Gaiman"], None, None),
            entry("American Gods", &["Neil Gaiman"], None, None),
        ];
        let groups = authors(&list);
        let names: Vec<Option<&str>> = groups.iter().map(|group| group.name.as_deref()).collect();
        assert_eq!(names, vec![Some("Neil Gaiman"), Some("Terry Pratchett")]);
        assert_eq!(groups[0].books, vec![1], "only the book that says so");
        assert_eq!(groups[1].books, vec![0], "the other sits under the first name");
    }

    #[test]
    fn an_authors_books_come_by_title_ignoring_a_leading_article() {
        assert_eq!(sortable("The Hobbit"), "hobbit");
        assert_eq!(sortable("Die Verwandlung"), "verwandlung");
        assert_eq!(sortable("Anathem"), "anathem");

        let list = vec![
            entry("Zebra", &["Tolkien"], None, None),
            entry("Anathem", &["Stephenson"], None, None),
            entry("The Hobbit", &["Tolkien"], None, None),
        ];
        let groups = authors(&list);
        let names: Vec<Option<&str>> = groups.iter().map(|group| group.name.as_deref()).collect();
        assert_eq!(names, vec![Some("Stephenson"), Some("Tolkien")]);
        // "The Hobbit" files under H, ahead of Zebra — an article is not a letter.
        assert_eq!(groups[1].books, vec![2, 0]);
    }

    #[test]
    fn the_author_filter_finds_names_the_way_the_book_filter_does() {
        let list = vec![
            entry("Norwegian Wood", &["Haruki Murakami"], None, None),
            entry("A Wizard of Earthsea", &["Ursula K. Le Guin"], None, None),
            entry("Anonymous", &[], None, None),
        ];
        let groups = authors(&list);

        // Case is folded away, and a word a name starts with comes first —
        // the same two passes `filter` runs over the books.
        assert_eq!(filter_authors(&groups, "MURAKAMI"), vec![0]);
        assert_eq!(filter_authors(&groups, "guin"), vec![1]);
        assert_eq!(filter_authors(&groups, ""), vec![0, 1, 2], "no word keeps everyone");
        assert!(filter_authors(&groups, "zzz").is_empty());
        // The nameless group has no name to match, so a word never picks it.
        assert!(!filter_authors(&groups, "a").contains(&2));
    }

    #[test]
    fn sort_books_ranks_by_author_then_title_with_the_nameless_last() {
        let mut list = vec![
            entry("Zebra", &["Tolkien"], None, None),
            entry("Anonymous", &[], None, None),
            entry("The Hobbit", &["Tolkien"], None, None),
            entry("Anathem", &["Stephenson"], None, None),
        ];
        sort_books(&mut list);
        let order: Vec<&str> = list
            .iter()
            .map(|entry| entry.record.title.as_deref().unwrap())
            .collect();
        assert_eq!(order, vec!["Anathem", "The Hobbit", "Zebra", "Anonymous"]);
    }

    #[test]
    fn a_filter_matches_anywhere_in_any_field() {
        let mut tagged = entry("Anathem", &["Stephenson"], None, None);
        tagged.record.tags = vec!["science fiction".into()];
        let list = vec![
            entry("The Hobbit", &["Tolkien"], Some("Middle-earth"), Some(1.0)),
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
            entry("Practical Fraud Prevention", &["Saporta"], None, None),
            entry("Understanding Eventsourcing", &["Dilger"], None, None),
        ];
        let found = filter(&list, "event");
        assert_eq!(found.len(), 1);
        assert_eq!(
            list[found[0]].record.title.as_deref(),
            Some("Understanding Eventsourcing")
        );
        let list = vec![entry("Understanding Eventsourcing", &["Dilger"], None, None)];
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
