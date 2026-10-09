//! Turning what the command line says into a book to read.
//!
//! A reference is a path, a book the library knows by id or by the words on
//! its spine, or a file `export` wrote — and one word may be any of the three.
//! Resolving it in one place keeps `open`, `inspect` and a search hit from each
//! deciding in their own way what a name means.

use crate::epub::Book;
use crate::export;
use crate::identity::BookId;
use crate::journal::{Journal, State};
use crate::library;
use crate::paths;
use crate::report::{counted, shown};
use crate::session::run_at;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// Opens the book an argument names: a file, or a book the library knows.
///
/// `inspect`, `images` and `blocks` used to take only a path and `show` only a
/// book the library holds, so one ordinary question could need two commands.
/// What each of them prints is unchanged; only what may be passed is wider.
pub(crate) fn open_argument(reference: &str) -> Result<Book> {
    let path = PathBuf::from(paths::expand_tilde(reference));
    if path.is_file() {
        return book_file(&path);
    }
    book_file(&library_file(reference)?)
}

/// The journal directory and the state replayed from it: the first two steps
/// every command takes before it can answer what a name means.
fn library_state() -> Result<(PathBuf, State)> {
    let journal_dir = paths::journal_dir()?;
    let state = Journal::replay(&journal_dir)?;
    Ok((journal_dir, state))
}

/// The file the journal recorded for a book it holds.
///
/// `unknown` is how the caller words a book the journal does not hold: `show`
/// says the journal knows nothing about it and `open` names the id, and both
/// messages are what a person sees, so they stay where they are written.
fn recorded_file(state: &State, id: &BookId, unknown: String) -> Result<PathBuf> {
    let record = state.book(id).with_context(|| unknown)?;
    record
        .path()
        .cloned()
        .with_context(|| format!("no file recorded for {}", record.display_title()))
}

/// The file a book the library knows lives in.
fn library_file(reference: &str) -> Result<PathBuf> {
    let (_, state) = library_state()?;
    let id = library::resolve(&state, reference)?;
    recorded_file(&state, &id, format!("the journal knows nothing about {id}"))
}

/// Opens a book file, saying which file when it cannot be read.
pub(crate) fn book_file(path: &Path) -> Result<Book> {
    Book::open(path).with_context(|| format!("cannot read {}", shown(path.display())))
}

/// Reads whatever a name points at.
///
/// A file on disk is a book file, and reading it adds it to the library as it
/// goes. Anything else is a book the library knows — by id, or by the words on
/// its spine — or a file `export` wrote, and `open_reference` sorts those out.
pub(crate) fn read_anything(reference: &str, chapter: Option<&str>, at: Option<&str>) -> Result<()> {
    let path = Path::new(reference);
    if path.is_file() && !reference.contains(".md") {
        let book = book_file(path)?;
        return run_at(
            book,
            path.to_path_buf(),
            chapter.map(str::to_string),
            at.map(str::to_string),
        );
    }
    open_reference(reference, chapter, at)
}

/// Opens a book at a place named by an exported file or a book id.
///
/// This is the way back from a search hit: the hit names a file, the file names
/// the book and chapter, and `--at` puts the reader on the passage rather than at
/// the top of a chapter.
fn open_reference(reference: &str, chapter: Option<&str>, at: Option<&str>) -> Result<()> {
    let (_, state) = library_state()?;

    // An id — or a long enough prefix of one — is the library's own answer,
    // and so is a name; `resolve` matches the way `show` and `forget` match, so
    // a book opens with whatever it is usually called. A file `export` wrote
    // carries its origin instead.
    let (book_id, from_file, from_line) = if names_a_file(reference) {
        origin_of_reference(reference)?
    } else {
        match library::resolve(&state, reference) {
            Ok(id) => (id.to_string(), None, None),
            // Nothing in the library answers to that name. If it looked like a
            // file, it was meant as one, and `origin_of` says what is wrong with
            // it; otherwise the library's own answer — which lists what it could
            // have meant — is the useful one.
            Err(err) if !reference.contains(".md") => return Err(err),
            Err(_) => origin_of_reference(reference)?,
        }
    };

    let id = BookId::from(book_id);
    let path = recorded_file(
        &state,
        &id,
        format!("no book with id {id} in the library"),
    )?;

    let book =
        Book::open(&path).with_context(|| format!("cannot read {}", shown(path.display())))?;
    let target = chapter.map(str::to_string).or(from_file);

    run_at(book, path, target, at.map(str::to_string).or(from_line))
}

/// Whether a reference names a file rather than a book.
///
/// A path that is there is a file `export` wrote, and so is anything under
/// `qmd://`. A name that merely ends in `.md` is still a name — a book may well
/// be called that — which is why the file is looked for rather than guessed at
/// from its extension.
fn names_a_file(reference: &str) -> bool {
    if reference.starts_with("qmd://") {
        return true;
    }
    let (file, _) = split_line_suffix(reference);
    std::path::Path::new(&paths::expand_tilde(file)).exists()
}

/// Where a file `export` wrote sends the reader: the book it came from, the
/// chapter, and the passage the reference names, if it names one.
fn origin_of_reference(reference: &str) -> Result<(String, Option<String>, Option<String>)> {
    let (path, line) = resolve_hit(reference)?;
    let origin = export::origin_of(&path)?;
    // A search hit names a line. Its text is the passage that matched, so it
    // makes a far better landing point than the top of a chapter.
    let text = line.and_then(|line| export::line_text(&path, line));
    Ok((origin.book, origin.chapter, text))
}

fn resolve_hit(reference: &str) -> Result<(PathBuf, Option<usize>)> {
    let (body, line) = split_line_suffix(reference);

    if let Some(rest) = body.strip_prefix("qmd://") {
        // Drop the collection name; what follows is relative to the export.
        let relative = rest.split_once('/').map(|(_, rest)| rest).unwrap_or(rest);
        let path = export::default_dir()?.join(relative);
        anyhow::ensure!(
            path.exists(),
            "{} does not exist. If the collection points elsewhere, pass the file path instead.",
            shown(path.display())
        );
        return Ok((path, line));
    }
    Ok((PathBuf::from(paths::expand_tilde(body)), line))
}

/// Splits a trailing `:123` off a reference, leaving a Windows-style drive letter
/// or a `sha256:` prefix alone.
fn split_line_suffix(reference: &str) -> (&str, Option<usize>) {
    match reference.rsplit_once(':') {
        Some((body, tail)) => match tail.parse::<usize>() {
            Ok(line) if !body.is_empty() => (body, Some(line)),
            _ => (reference, None),
        },
        None => (reference, None),
    }
}

/// Reads a directory into the library.
pub(crate) fn scan_directory(dir: &Path, filenames: bool) -> Result<()> {
    // A path that is not a directory is a typo half the time, and `0 files seen`
    // with a zero exit is how it went unnoticed.
    if !dir.is_dir() {
        bail!("{} is not a directory to scan", shown(dir.display()));
    }
    let (journal_dir, state) = library_state()?;
    let mut journal = Journal::open(&journal_dir)?;

    println!("scanning {} ...", shown(dir.display()));
    let mut count = 0usize;
    let report = library::scan(dir, &mut journal, &state, filenames, &mut |path| {
        count += 1;
        // A scan over hundreds of files should say that it is working.
        if count.is_multiple_of(25) {
            println!("  {count} files ... {}", shown(path.display()));
        }
    })?;

    println!(
        "\n{} seen, {} added, {} moved",
        counted(report.seen, "file"),
        report.added,
        report.moved
    );
    if !report.unreadable.is_empty() {
        println!(
            "\n{} could not be read:",
            counted(report.unreadable.len(), "file")
        );
        for (path, err) in &report.unreadable {
            // A downloaded file may be named for the terminal rather than for
            // a person: the path and the message are cleaned together.
            println!("{}", shown(format!("  {}: {err}", path.display())));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_name_that_ends_in_md_is_still_a_name() {
        // `open` once treated anything ending in `.md` as a file `export` wrote,
        // so a book called Draft.md could not be opened by its name at all: the
        // export reader answered "carries no book reference".
        assert!(!names_a_file("Draft.md"), "a book may be called that");
        assert!(
            names_a_file("qmd://books/Draft.md:12"),
            "a qmd reference is a file"
        );
        let here = stub_file("omaread-a-file", ":");
        assert!(
            names_a_file(&here.to_string_lossy()),
            "a file that is there is a file"
        );
        std::fs::remove_file(&here).ok();
    }

    /// Writes an executable stub file, for a test that needs a path that exists.
    fn stub_file(name: &str, body: &str) -> PathBuf {
        let path = crate::testkit::path(&format!("stub-{name}"), "");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
}
