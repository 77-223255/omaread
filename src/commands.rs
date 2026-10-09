//! The subcommands: what each one answers, and what it writes.
//!
//! Everything the command line can ask for that is not "read this book" lives
//! here, so the dispatch in `main` stays a table of names and the bodies stay
//! together — a correction written by `set` and the same correction written by
//! `edit` are one function, and cannot learn to write differently.

use crate::cli::Fields;
use crate::doc;
use crate::epub::Book;
use crate::export;
use crate::identity::BookId;
use crate::journal::{self, Journal, Payload, State};
use crate::library;
use crate::measure;
use crate::paths;
use crate::report::{counted, field, headline, or_dash, print_json, record_json, shown};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// Takes books out of the library, and everything recorded about them with it.
///
/// The events stay in the journal — it only ever grows — but replaying it no
/// longer finds the book: no record, no reading position. What a scan adds next
/// is a book nobody has read.
///
/// What picks the books is a path when the path exists — that is how a person
/// points at a shelf, and a folder means everything under it — and a book id or a
/// title otherwise.
pub(crate) fn forget(reference: &str, json: bool) -> Result<()> {
    let journal_dir = paths::journal_dir()?;
    let state = Journal::replay(&journal_dir)?;

    let as_path = PathBuf::from(paths::expand_tilde(reference));
    if as_path.exists() {
        return forget_under(&as_path, &state, &journal_dir, json);
    }

    let id = library::resolve(&state, reference)?;
    let title = state
        .book(&id)
        .map(|record| record.display_title())
        .unwrap_or_default();

    let mut journal = Journal::open(&journal_dir)?;
    journal.append(&id, Payload::BookForgotten)?;
    if json {
        print_json(&serde_json::json!([{ "id": id.as_str(), "title": title }]))?;
        return Ok(());
    }
    println!("forgot {title}");
    Ok(())
}

/// Forgets every book the library holds under a directory, or the one file that
/// was named.
fn forget_under(root: &Path, state: &State, journal_dir: &Path, json: bool) -> Result<()> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());

    // A directory with the home directory inside it is not a shelf: it is a typo,
    // or a variable that did not expand. Forgetting under it would take the whole
    // library in one keystroke, and a forgotten book is removed rather than
    // hidden — the reading position goes with it, and nothing puts it back. A
    // shelf *under* the home directory is what this is for; the home directory,
    // and anything above it, is not.
    if let Some(home) = dirs::home_dir()
        && home.starts_with(&root) {
            bail!(
                "{} holds your home directory: name the shelf inside it",
                shown(root.display())
            );
        }
    let chosen: Vec<(BookId, String)> = state
        .books()
        .filter(|(_, record)| record.paths.iter().any(|path| path.starts_with(&root)))
        .map(|(id, record)| (id.clone(), record.display_title()))
        .collect();

    if chosen.is_empty() {
        bail!("nothing in the library is under {}", shown(root.display()));
    }

    let mut journal = Journal::open(journal_dir)?;
    for (id, _) in &chosen {
        journal.append(id, Payload::BookForgotten)?;
    }

    if json {
        let items: Vec<serde_json::Value> = chosen
            .iter()
            .map(|(id, title)| serde_json::json!({ "id": id.as_str(), "title": title }))
            .collect();
        print_json(&items)?;
        return Ok(());
    }

    println!(
        "forgot {} under {}",
        counted(chosen.len(), "book"),
        shown(root.display())
    );
    for (_, title) in chosen.iter().take(12) {
        println!("  {title}");
    }
    if chosen.len() > 12 {
        println!("  … and {} more", chosen.len() - 12);
    }
    Ok(())
}

/// Reads or corrects what a book says about itself.
///
/// The name in the library is the one inside the file, which is right far more
/// often than the file's own name is — and wrong often enough to need fixing: a
/// title that is a string of junk, a series nobody recorded, an author split in
/// two. The correction is a journal event, so it wins over the file, survives a
/// rescan, and never writes anything into the EPUB.
pub(crate) fn metadata(reference: &str, fields: &Fields, json: bool) -> Result<()> {
    let journal_dir = paths::journal_dir()?;
    let state = Journal::replay(&journal_dir)?;

    // A path names a book as an id or a title does. The library's record is
    // the answer when it has seen the file — corrections and all — and what
    // the file itself says when it never has.
    let path = PathBuf::from(paths::expand_tilde(reference));
    if path.is_file() {
        // The journal answers by path first, so a book recorded here needs no
        // hashing at all: hashing is a pass over the whole file, and `show`
        // is pointed at files a person is looking at. The hash runs only when
        // no record claims this path — and runs once, the id computed here
        // and handed on rather than asked for again below.
        let here = path.canonicalize().unwrap_or_else(|_| path.clone());
        if let Some(id) = state
            .books()
            .find(|(_, record)| record.paths.iter().any(|p| *p == here || *p == path))
            .map(|(id, _)| id.clone())
        {
            return known_book(&state, &journal_dir, &id, fields, json);
        }
        let id = BookId::of_file(&path)?;
        if state.book(&id).is_some() {
            return known_book(&state, &journal_dir, &id, fields, json);
        }
        if fields.is_empty() {
            return show_unseen_file(&path, &id, json);
        }
        bail!(
            "the library has never seen {}, so there is nothing to correct",
            shown(path.display())
        );
    }

    let id = library::resolve(&state, reference)?;
    known_book(&state, &journal_dir, &id, fields, json)
}

/// Applies one correction to every book a word picks out.
///
/// One book at a time is how a person fixes a title; an agent normalising a
/// name a dozen files spell a dozen ways wants one call. The matcher is the one
/// `list --filter` uses, so a preview is always `list --filter WORD --json`
/// away.
pub(crate) fn edit_matched(needle: &str, fields: &Fields, json: bool) -> Result<()> {
    let journal_dir = paths::journal_dir()?;
    let state = Journal::replay(&journal_dir)?;
    let all = library::entries(&state);
    let matched = library::filter(&all, needle);
    if matched.is_empty() {
        bail!("{}", library::no_match(needle));
    }

    let changes = fields.changes();
    let ids: std::collections::HashSet<BookId> =
        matched.iter().map(|&index| all[index].id.clone()).collect();
    let mut journal = Journal::open(&journal_dir)?;
    for id in &ids {
        journal.append(id, changes.clone())?;
    }

    if json {
        // What the books read as now, so the caller sees what it wrote.
        let after = Journal::replay(&journal_dir)?;
        let records: Vec<serde_json::Value> = library::entries(&after)
            .iter()
            .filter(|entry| ids.contains(&entry.id))
            .map(|entry| record_json(&entry.id, &entry.record, &after))
            .collect();
        print_json(&records)?;
        return Ok(());
    }
    println!(
        "corrected {} matching {}:",
        counted(ids.len(), "book"),
        shown(needle)
    );
    for &index in &matched {
        println!("  {}", shown(all[index].record.display_title()));
    }
    Ok(())
}

/// Every distinct author name, with the number of books it names.
///
/// The same author is written several ways across books — "Murakami, Haruki"
/// here, "Haruki Murakami" there, a Chinese form somewhere else — and nothing
/// in the files ties them together. This is the view that shows the mess, so a
/// correction can be aimed at all of one spelling with `edit`.
pub(crate) fn list_authors(json: bool) -> Result<()> {
    let journal_dir = paths::journal_dir()?;
    let state = Journal::replay(&journal_dir)?;
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for entry in library::entries(&state) {
        for author in &entry.record.authors {
            let name = author.trim();
            if !name.is_empty() {
                *counts.entry(name.to_string()).or_default() += 1;
            }
        }
    }

    let mut rows: Vec<(String, usize)> = counts.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));

    if json {
        let items: Vec<serde_json::Value> = rows
            .iter()
            .map(|(name, books)| serde_json::json!({ "author": name, "books": books }))
            .collect();
        print_json(&items)?;
        return Ok(());
    }
    if rows.is_empty() {
        println!("no authors in the library");
        return Ok(());
    }
    for (name, books) in &rows {
        println!("{books:>4}  {}", shown(name));
    }
    Ok(())
}

/// Shows what the library holds about one book, or records the correction
/// asked for. Both live here because `show` is `known_book` with no fields to
/// set: one path, so what is printed and what is written cannot drift apart.
fn known_book(
    state: &State,
    journal_dir: &Path,
    id: &BookId,
    fields: &Fields,
    json: bool,
) -> Result<()> {
    let changes = fields.changes();

    if fields.is_empty() {
        let record = state
            .book(id)
            .with_context(|| format!("the journal knows nothing about {id}"))?;
        if json {
            print_json(&record_json(id, record, state))?;
            return Ok(());
        }
        // Every field on a line of its own, and a value that is not there
        // reads `-`: one rule, so one absence looks the same as in `list` and
        // `inspect`. Every value is cleaned on the way out: what a book says
        // is shown, not obeyed.
        field("title:", or_dash(record.title.clone().unwrap_or_default()), 10);
        field("authors:", record.display_authors(), 10);
        field(
            "series:",
            or_dash(record.series_label().unwrap_or_default()),
            10,
        );
        field("tags:", or_dash(record.tags.join(", ")), 10);
        field(
            "rating:",
            or_dash(record.rating.map(|r| r.to_string()).unwrap_or_default()),
            10,
        );
        field(
            "publisher:",
            or_dash(record.publisher.clone().unwrap_or_default()),
            10,
        );
        field(
            "year:",
            or_dash(record.year.map(|y| y.to_string()).unwrap_or_default()),
            10,
        );
        field(
            "language:",
            or_dash(record.language.clone().unwrap_or_default()),
            10,
        );
        field(
            "file:",
            or_dash(
                record
                    .path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            ),
            10,
        );
        field("id:", id.to_string(), 10);
        return Ok(());
    }

    let mut journal = Journal::open(journal_dir)?;
    journal.append(id, changes)?;
    if json {
        // What the book reads as now, so the caller sees what it wrote.
        let after = Journal::replay(journal_dir)?;
        let record = after
            .book(id)
            .with_context(|| format!("the journal knows nothing about {id}"))?;
        print_json(&record_json(id, record, &after))?;
        return Ok(());
    }
    println!("corrected; the library shows this from now on:");
    println!("  omaread show {id}");
    Ok(())
}

/// What a file says about itself, for a book the library has never seen.
///
/// The same lines `show` prints for a book it knows, so the two answers read
/// against each other; the fields a file does not hold read `-`. The id is
/// handed in rather than computed again: the caller has already hashed the
/// file once to get this far.
fn show_unseen_file(path: &Path, id: &BookId, json: bool) -> Result<()> {
    let book =
        Book::open(path).with_context(|| format!("cannot read {}", shown(path.display())))?;
    if json {
        let mut record = journal::BookRecord::new(path.to_path_buf());
        record.title = book.metadata.title;
        record.authors = book.metadata.authors;
        record.language = book.metadata.language;
        print_json(&record_json(id, &record, &State::default()))?;
        return Ok(());
    }
    // Every value cleaned on the way out: a file's own title is shown, not
    // obeyed.
    // What a file does not hold reads the same dash as a field nobody filled
    // in the library.
    let absent = || "-".to_string();
    field("title:", or_dash(book.metadata.title.unwrap_or_default()), 10);
    field("authors:", or_dash(book.metadata.authors.join(", ")), 10);
    field("series:", absent(), 10);
    field("tags:", absent(), 10);
    field("rating:", absent(), 10);
    field("publisher:", absent(), 10);
    field("year:", absent(), 10);
    field(
        "language:",
        or_dash(book.metadata.language.unwrap_or_default()),
        10,
    );
    field("file:", path.display().to_string(), 10);
    field("id:", id.to_string(), 10);
    Ok(())
}

/// Shows how big the log is and how much of it still matters.
pub(crate) fn journal_status(json: bool) -> Result<()> {
    let status = journal::status(&paths::journal_dir()?)?;
    if json {
        print_json(&status)?;
        return Ok(());
    }
    for file in &status.files {
        println!(
            "{:<32} {:>7} events  {:>9} bytes",
            file.name, file.events, file.bytes
        );
    }
    println!(
        "{} events in {} files, {} bytes; {} no longer matter",
        status.events,
        status.files.len(),
        status.bytes,
        status.dead
    );
    Ok(())
}

/// Folds this machine's log, and says how much it lost.
pub(crate) fn journal_compact(json: bool) -> Result<()> {
    let compact = journal::compact(&paths::journal_dir()?)?;
    if json {
        print_json(&compact)?;
        return Ok(());
    }
    println!(
        "folded {} events to {} ({} dropped)",
        compact.before,
        compact.after,
        compact.before - compact.after
    );
    Ok(())
}

/// Writes the library out as Markdown.
pub(crate) fn export_library(dir: &str, force: bool, reindex: bool, embed: bool) -> Result<()> {
    let journal_dir = paths::journal_dir()?;
    let state = Journal::replay(&journal_dir)?;
    let dir = if dir.is_empty() {
        export::default_dir()?
    } else {
        PathBuf::from(paths::expand_tilde(dir))
    };

    println!("exporting to {} ...", shown(dir.display()));
    let report = export::export(&dir, &state, force)?;
    println!(
        "\n{}, {} written; {} unchanged",
        counted(report.books, "book"),
        counted(report.chapters, "chapter"),
        report.unchanged
    );
    if !report.skipped.is_empty() {
        println!(
            "\n{} could not be read:",
            counted(report.skipped.len(), "file")
        );
        for (slug, err) in report.skipped.iter().take(10) {
            println!("{}", shown(format!("  {slug}: {err}")));
        }
    }
    if reindex {
        return run_qmd(&dir, embed);
    }
    // Only when something was actually written: a run that changed nothing
    // has nothing to index, and the advice would be six lines to dismiss.
    for line in indexing_advice(&dir, report.chapters > 0) {
        println!("{line}");
    }
    Ok(())
}

/// The command that points qmd at an export directory, spelled once so the
/// several places that print it cannot drift apart.
fn qmd_collection_hint(dir: &Path) -> String {
    format!("  qmd collection add {} --name books", shown(dir.display()))
}

/// The one way to say the library has nothing in it, wherever a command finds
/// it empty.
pub(crate) fn empty_library_hint() {
    println!("The library is empty. Read one in with:");
    println!("  omaread scan ~/path/to/books");
}

/// The advice that follows an export: how to hand what it wrote to qmd, and
/// the command that does both halves itself.
///
/// Empty when nothing was written, which is when there is nothing to say.
fn indexing_advice(dir: &Path, wrote: bool) -> Vec<String> {
    if !wrote {
        return Vec::new();
    }
    vec![
        String::new(),
        "To index it:".to_string(),
        qmd_collection_hint(dir),
        "  qmd embed".to_string(),
        String::new(),
        "Or let omaread do it: omaread export --reindex [--embed]".to_string(),
    ]
}

/// Hands the export to qmd for indexing.
///
/// `qmd update` re-indexes the collections it already knows. A first run has none,
/// so a failure is not an error here: it means the collection has to be created,
/// and that is a decision about naming which belongs to the user.
fn run_qmd(dir: &Path, embed: bool) -> Result<()> {
    println!("\nrunning qmd update ...");
    let status = std::process::Command::new("qmd").arg("update").status();
    match status {
        Ok(status) if status.success() => {}
        Ok(_) => {
            println!("\nqmd update did not succeed. If no collection points here yet:");
            println!("{}", qmd_collection_hint(dir));
            return Ok(());
        }
        Err(err) => {
            println!("\ncannot run qmd: {err}");
            println!("{}", qmd_collection_hint(dir));
            return Ok(());
        }
    }

    if embed {
        println!("\nrunning qmd embed ...");
        // Embedding runs a local model over everything new; it is slow by nature,
        // which is why it takes a flag of its own.
        let _ = std::process::Command::new("qmd").arg("embed").status();
    } else {
        println!("\nFor semantic search, update the vectors as well:");
        println!("  qmd embed");
    }
    Ok(())
}

/// Prints the library.
pub(crate) fn list_library(as_json: bool, needle: &str) -> Result<()> {
    let state = Journal::replay(&paths::journal_dir()?)?;
    let mut all = library::entries(&state);
    // Author then title: the order the shelf browses in, so a list read on
    // the terminal and a shelf opened on it agree about what comes first.
    library::sort_books(&mut all);
    let matched = library::filter(&all, needle);
    // The books themselves are read out of `all` once: the matcher answers
    // which rows, and copying the rows to hold them would clone the library a
    // second time for a command that only prints.
    let entries: Vec<&library::Entry> = matched.iter().map(|&index| &all[index]).collect();

    if as_json {
        return print_books(&entries, &state);
    }
    if entries.is_empty() {
        if needle.trim().is_empty() {
            empty_library_hint();
        } else {
            println!("{}", library::no_match(needle));
        }
        return Ok(());
    }

    println!("{}\n", headline(entries.len(), needle));
    for entry in &entries {
        let record = &entry.record;
        // The series sits where a series would be named, and where there is
        // none the column holds the same dash as every other absent value.
        let series = match record.series_label() {
            Some(label) => format!("  [{label}]"),
            None => "  -".to_string(),
        };
        println!(
            "  {} {}{series}",
            measure::pad(&record.display_title(), 34),
            measure::pad(&record.display_authors(), 26)
        );
    }
    Ok(())
}

/// Writes books as JSON, one object each, for a program to read.
fn print_books(entries: &[&library::Entry], state: &State) -> Result<()> {
    let books: Vec<serde_json::Value> = entries
        .iter()
        .map(|entry| record_json(&entry.id, &entry.record, state))
        .collect();

    print_json(&books)?;
    Ok(())
}

/// Prints one chapter's blocks with their kind, to see how markup came through.
pub(crate) fn dump_chapter(mut book: Book, number: usize) -> Result<()> {
    // The number as a person counts it, checked against this very book: the
    // spine is the only thing that says which chapters exist.
    let index = spine_index(number, book.spine.len())?;
    let chapter = book.chapter(index)?;
    println!(
        "{} - {}, {}, {}\n",
        shown(chapter.href),
        counted(chapter.blocks.len(), "block"),
        counted(chapter.links.len(), "link"),
        counted(chapter.anchors.len(), "anchor")
    );
    for link in &chapter.links {
        println!(
            "  link  block {:>4} chars {}..{}  -> {}",
            link.block,
            link.start,
            link.end,
            shown(&link.target)
        );
    }
    if !chapter.links.is_empty() {
        println!();
    }
    for (n, block) in chapter.blocks.iter().enumerate() {
        let kind = match &block.kind {
            doc::BlockKind::Heading(level) => format!("h{level}"),
            doc::BlockKind::Paragraph => "p".to_string(),
            doc::BlockKind::Quote => "quote".to_string(),
            doc::BlockKind::Code => "code".to_string(),
            doc::BlockKind::ListItem { depth, ordinal } => {
                format!("li d{depth} {ordinal:?}")
            }
            doc::BlockKind::Rule => "rule".to_string(),
            doc::BlockKind::Image { src } => {
                format!(
                    "img {}",
                    src.as_deref().map(shown).unwrap_or_else(|| "-".into())
                )
            }
        };
        let text = block.plain_text();
        println!(
            "{n:>4} {kind:<12} {:>4}ch  {}",
            text.chars().count(),
            measure::fit(&text, 90)
        );
    }
    Ok(())
}

/// Every picture a book holds, and how big it is.
///
/// The size printed is the picture's own, in pixels — not the size it would be
/// drawn at, which depends on the terminal's cells and on how much room the view
/// gives it: that belongs to the reader, not to a report about a file. A picture
/// whose header cannot be read is named rather than guessed at.
pub(crate) fn list_images(mut book: Book) -> Result<()> {
    let mut total = 0;
    let mut readable = 0;

    for index in 0..book.spine.len() {
        let Ok(chapter) = book.chapter(index) else {
            continue;
        };
        let sources: Vec<(usize, Option<String>)> = chapter
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(block, b)| match &b.kind {
                doc::BlockKind::Image { src } => Some((block, src.clone())),
                _ => None,
            })
            .collect();

        for (block, src) in sources {
            total += 1;
            let Some(src) = src else {
                println!("  ch{index:>3} block{block:>4}  NO SRC");
                continue;
            };
            let shown_src = shown(&src);
            let bytes = match book.read_binary(&src) {
                Ok(bytes) => bytes,
                Err(err) => {
                    println!(
                        "  ch{index:>3} block{block:>4}  MISSING      {shown_src}: {}",
                        shown(err)
                    );
                    continue;
                }
            };
            let Ok((width, height)) = crate::image::dimensions(&bytes) else {
                println!("  ch{index:>3} block{block:>4}  UNREADABLE   {shown_src}");
                continue;
            };
            readable += 1;
            println!(
                "  ch{index:>3} block{block:>4}  {:>8} B  {width}x{height} px  {shown_src}",
                bytes.len()
            );
        }
    }

    println!("\n{total} images, {readable} readable");
    Ok(())
}

/// Prints metadata and the reading order. Useful to check a book without the
/// full interface.
pub(crate) fn inspect(mut book: Book) -> Result<()> {
    // Every value cleaned on the way out — a book's own fields and paths are
    // shown, not obeyed, the same way `show` prints them — and an absent one
    // reads `-`, whatever the book left out.
    field(
        "title:",
        or_dash(book.metadata.title.clone().unwrap_or_default()),
        11,
    );
    field("authors:", or_dash(book.metadata.authors.join(", ")), 11);
    field(
        "language:",
        or_dash(book.metadata.language.clone().unwrap_or_default()),
        11,
    );
    field(
        "identifier:",
        or_dash(book.metadata.identifier.clone().unwrap_or_default()),
        11,
    );
    // What was found for pictures, so a cover that draws small can be traced
    // to a book that never named one rather than to the drawing. A book that
    // declared no layout reads `-`: reflowable is the default, and printing
    // it as a declaration claimed more than the book said.
    field("layout:", or_dash(book.layout.clone().unwrap_or_default()), 11);
    field("cover:", or_dash(book.cover.clone().unwrap_or_default()), 11);
    println!("spine:      {}\n", counted(book.spine.len(), "item"));

    let count = book.spine.len();
    let mut failures = 0;
    let mut blocks_total = 0;
    for index in 0..count {
        let item = book.spine[index].clone();
        match book.chapter(index) {
            Ok(chapter) => {
                blocks_total += chapter.blocks.len();
                println!(
                    "  {:>3}. {:<48} {:>4} blocks  {}",
                    index + 1,
                    measure::fit(&shown(item.title.as_deref().unwrap_or(&item.href)), 48),
                    chapter.blocks.len(),
                    shown(item.href)
                );
            }
            Err(err) => {
                failures += 1;
                // The whole line, error chain included: an entry's own name
                // is part of what is printed.
                println!(
                    "{}",
                    shown(format!("  {:>3}. FAILED {}: {err:#}", index + 1, item.href))
                );
            }
        }
    }
    println!("\n{}", inspect_summary(blocks_total, failures));
    Ok(())
}

/// The last line of an inspect: the blocks, always, and the chapters that
/// failed only when some did — a count of zero has nothing to tell anyone.
fn inspect_summary(blocks: usize, failures: usize) -> String {
    match failures {
        0 => format!("{} total", counted(blocks, "block")),
        _ => format!(
            "{} total, {} failed",
            counted(blocks, "block"),
            counted(failures, "chapter")
        ),
    }
}

/// A chapter the command line was given: its number in the spine, or an href.
pub(crate) enum Chapter {
    /// The number as typed, counted from 1.
    Number(usize),
    Href(String),
}

/// Reads what `--chapter` says against the book it is for.
///
/// A number is the chapter's place in the spine — the same number `blocks`
/// takes — so `--chapter 3` opens the third chapter instead of hunting for an
/// href written "3". Anything else is an href, as it always was.
pub(crate) fn chapter_of(book: &Book, spec: &str) -> Result<Chapter> {
    let trimmed = spec.trim();
    if let Ok(number) = trimmed.parse::<usize>() {
        spine_index(number, book.spine.len())?;
        return Ok(Chapter::Number(number));
    }
    Ok(Chapter::Href(spec.to_string()))
}

/// A chapter number counted from 1, against the number of chapters there are.
///
/// Zero and everything past the end are not chapters, and naming the numbers
/// that exist beats opening the first chapter silently — which is what
/// `blocks 0` used to do with it.
fn spine_index(number: usize, chapters: usize) -> Result<usize> {
    anyhow::ensure!(
        (1..=chapters).contains(&number),
        "no chapter {number}: this book numbers its chapters from 1 to {chapters}"
    );
    Ok(number - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chapter_number_counts_from_one_and_stops_at_the_spine() {
        assert_eq!(spine_index(1, 3).unwrap(), 0, "the first chapter is first");
        assert_eq!(spine_index(3, 3).unwrap(), 2, "the last is the last");
        // Zero used to be read as the first chapter without a word.
        let err = spine_index(0, 3).unwrap_err().to_string();
        assert!(err.contains("no chapter 0"), "{err}");
        assert!(err.contains("from 1 to 3"), "{err}");
        // And past the end used to be clamped to something that exists.
        let err = spine_index(4, 3).unwrap_err().to_string();
        assert!(err.contains("no chapter 4"), "{err}");
        assert!(err.contains("from 1 to 3"), "{err}");
    }

    #[test]
    fn the_advice_after_an_export_only_comes_when_something_was_written() {
        let dir = Path::new("/tmp/export");
        assert!(
            indexing_advice(dir, false).is_empty(),
            "nothing written, nothing to index"
        );
        let advice = indexing_advice(dir, true);
        assert!(
            advice.iter().any(|line| line.contains("qmd collection add")),
            "the advice names the command that creates the collection: {advice:?}"
        );
        // The advice names the command it belongs to, not a bare flag.
        assert!(
            advice
                .iter()
                .any(|line| line.contains("omaread export --reindex")),
            "{advice:?}"
        );
    }
}
