//! Writing books out as Markdown.
//!
//! The point is not to convert books for their own sake, but to feed a search
//! engine. qmd indexes Markdown collections with BM25 and embeddings; giving it
//! one file per chapter saves building an index here, and it makes the whole
//! library searchable by meaning rather than only by substring.
//!
//! Every file carries front matter naming the book and the chapter it came from,
//! so a hit can be turned back into a place in the reader. Without that the
//! search would be a dead end: it would tell you that something exists, but not
//! where to read it.

use crate::doc::{BlockKind, Chapter};
use crate::epub::Book;
use crate::identity::BookId;
use crate::journal::{BookRecord, State};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct Report {
    pub books: usize,
    pub chapters: usize,
    pub skipped: Vec<(String, String)>,
    /// Books left alone because nothing changed since the last export.
    pub unchanged: usize,
}

/// Writes the library to `dir`, one directory per book.
///
/// `force` writes even where the export is newer than the book, which is
/// otherwise skipped: re-exporting a whole library on every run would make the
/// command useless in a loop.
pub fn export(dir: &Path, state: &State, force: bool) -> Result<Report> {
    let mut report = Report::default();
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;

    let mut books: Vec<(BookId, BookRecord)> = state
        .books()
        .map(|(id, record)| (id.clone(), record.clone()))
        .collect();
    // A stable order keeps the output diffable.
    books.sort_by_key(|(_, record)| record.display_title().to_lowercase());

    for (id, record) in books {
        let Some(path) = record.path().cloned() else {
            continue;
        };
        let slug = slug_of(&record, &id);
        let book_dir = dir.join(&slug);

        if !force && is_current(&book_dir, &path) {
            report.unchanged += 1;
            continue;
        }

        let mut book = match Book::open(&path) {
            Ok(book) => book,
            Err(err) => {
                report.skipped.push((slug, format!("{err:#}")));
                continue;
            }
        };
        std::fs::create_dir_all(&book_dir)?;

        // A stale export must not linger: chapters can vanish when a book is
        // replaced by another edition.
        clear_chapters(&book_dir)?;

        let count = book.spine.len();
        for index in 0..count {
            let title = book.chapter_title(index);
            let Ok(chapter) = book.chapter(index) else {
                continue;
            };
            let body =
                chapter_markdown_titled(&chapter, Some(&record.display_title()), Some(&title));
            if body.trim().is_empty() {
                continue;
            }
            let file = book_dir.join(format!("{:03}-{}.md", index + 1, slugify(&title)));
            let text = format!(
                "{}\n{body}",
                front_matter(&[
                    ("book", id.as_str()),
                    ("title", &record.display_title()),
                    ("author", &record.display_authors()),
                    ("chapter", &chapter.href),
                    ("chapter_title", &title),
                    ("chapter_number", &(index + 1).to_string()),
                ])
            );
            // An export names what somebody reads, so a file written now is
            // created private, like the journal — and an export left over
            // from an older run is tightened rather than left open.
            write_private(&file, &text)
                .with_context(|| format!("cannot write {}", file.display()))?;
            report.chapters += 1;
        }

        report.books += 1;
    }

    Ok(report)
}

/// Modification time of a path, if it can be read.
fn newest(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Writes a file only its owner can read, the way the journal is written.
///
/// The mode applies only when a file is created, so an existing file is
/// tightened afterwards as well: an export from an older run does not keep
/// looser permissions.
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(text.as_bytes())?;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    Ok(())
}

/// True when the exported chapters are at least as new as the book file.
fn is_current(book_dir: &Path, book: &Path) -> bool {
    let Some(source) = newest(book) else {
        return false;
    };
    let Ok(entries) = std::fs::read_dir(book_dir) else {
        return false;
    };
    entries
        .flatten()
        .filter_map(|entry| newest(&entry.path()))
        .max()
        .is_some_and(|exported| exported >= source)
}

/// Removes every Markdown file the book directory holds, so a chapter that is
/// gone from this edition cannot linger as if it were still current.
fn clear_chapters(dir: &Path) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("md") {
            std::fs::remove_file(path).ok();
        }
    }
    Ok(())
}

/// YAML front matter, as qmd and most Markdown tools expect it.
fn front_matter(fields: &[(&str, &str)]) -> String {
    let mut out = String::from("---\n");
    for (key, value) in fields {
        out.push_str(&format!("{key}: {}\n", quote(value)));
    }
    out.push_str("---\n");
    out
}

/// Quotes a YAML scalar. Colons and leading characters would otherwise change
/// the meaning of a line, and book titles are full of colons.
fn quote(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{}\"", escaped.replace('\n', " "))
}

/// Turns a chapter into Markdown.
///
/// The aim is searchable prose, not a faithful copy: headings keep their level so
/// a hit can be placed in the book's structure, code stays fenced so it is not
/// mistaken for text, and images become their alt text.
///
/// A title line naming book and chapter is added when one is given. Search
/// engines show a document's first heading as the title of a hit, and "34" or
/// "Pattern: Snapshots" alone says nothing about the book. The chapter's own
/// headings move down one level so the document has a single title with the
/// chapter's structure beneath it. Replacing the first heading instead would go
/// wrong wherever a book splits number and title across two of them.
pub fn chapter_markdown_titled(
    chapter: &Chapter,
    book_title: Option<&str>,
    chapter_title: Option<&str>,
) -> String {
    let mut out = String::new();
    let mut in_code = false;
    // Only shift when a title of our own is added, or the levels would be wrong
    // for anyone reading the file on its own.
    let shift = u8::from(book_title.is_some() || chapter_title.is_some());

    if shift == 1 {
        let title = match (book_title, chapter_title) {
            (Some(book), Some(chapter)) => format!("{book} · {chapter}"),
            (Some(book), None) => book.to_string(),
            (None, Some(chapter)) => chapter.to_string(),
            (None, None) => unreachable!("shift is set only when one is given"),
        };
        out.push_str(&format!("# {title}\n\n"));
    }

    for block in &chapter.blocks {
        let text = block.plain_text();
        let is_code = block.kind == BlockKind::Code;

        if is_code && !in_code {
            out.push_str("\n```\n");
            in_code = true;
        } else if !is_code && in_code {
            out.push_str("```\n\n");
            in_code = false;
        }

        match &block.kind {
            BlockKind::Heading(level) => {
                let hashes = "#".repeat(level.saturating_add(shift).clamp(1, 6) as usize);
                out.push_str(&format!("\n{hashes} {text}\n\n"));
            }
            BlockKind::Code => {
                out.push_str(&text);
                out.push('\n');
            }
            BlockKind::Quote => out.push_str(&format!("> {text}\n\n")),
            BlockKind::ListItem { depth, ordinal } => {
                let indent = "  ".repeat(*depth as usize);
                match ordinal {
                    Some(n) => out.push_str(&format!("{indent}{n}. {text}\n")),
                    None => out.push_str(&format!("{indent}- {text}\n")),
                }
            }
            BlockKind::Rule => out.push_str("\n---\n\n"),
            BlockKind::Image { .. } => {
                if !text.trim().is_empty() {
                    out.push_str(&format!("![{text}]()\n\n"));
                }
            }
            BlockKind::Paragraph => {
                if !text.trim().is_empty() {
                    out.push_str(&format!("{text}\n\n"));
                }
            }
        }
    }
    if in_code {
        out.push_str("```\n");
    }
    out
}

/// Directory name for a book: its title, plus enough of the hash to keep two
/// books of the same title apart.
fn slug_of(record: &BookRecord, id: &BookId) -> String {
    let short: String = id.as_str().chars().skip("sha256:".len()).take(8).collect();
    format!("{}-{short}", slugify(&record.display_title()))
}

/// Lowercase, ASCII, hyphens. A file name that survives every filesystem.
pub fn slugify(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_dash = true;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    // Long titles would make unwieldy paths.
    let cut: String = trimmed.chars().take(60).collect();
    if cut.is_empty() {
        "untitled".to_string()
    } else {
        cut.trim_matches('-').to_string()
    }
}

/// What an exported file says about where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub book: String,
    pub chapter: Option<String>,
}

/// Reads the front matter of an exported file. This is what turns a search hit
/// back into a place in a book.
pub fn origin_of(path: &Path) -> Result<Origin> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    parse_origin(&text).with_context(|| format!("{} carries no book reference", path.display()))
}

/// The text of one line of an exported file, trimmed to something worth
/// showing as the passage a hit landed on.
///
/// A whole line can be a long paragraph; the first words are enough to find
/// the passage again and are less likely to differ from the book by a stray
/// character. Front matter is bookkeeping rather than book text, and one
/// word on a line is no passage — the search hands a hit back to the reader
/// with it, and both callers want the same answer.
pub fn line_text(path: &Path, line: usize) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let raw = text.lines().nth(line.saturating_sub(1))?.trim();
    if raw.is_empty() || raw.starts_with("---") {
        return None;
    }
    // Markdown decoration would not appear in the book's own text.
    let cleaned = raw.trim_start_matches(['#', '>', '-', '*', ' ']).trim();
    let words: Vec<&str> = cleaned.split_whitespace().take(8).collect();
    if words.len() < 2 {
        None
    } else {
        Some(words.join(" "))
    }
}

fn parse_origin(text: &str) -> Option<Origin> {
    let body = text.strip_prefix("---")?;
    let end = body.find("\n---")?;
    let mut book = None;
    let mut chapter = None;
    for line in body[..end].lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches('"').to_string();
        match key.trim() {
            "book" => book = Some(value),
            "chapter" => chapter = Some(value),
            _ => {}
        }
    }
    Some(Origin {
        book: book?,
        chapter,
    })
}

/// Default place for the export, beside the read model.
pub fn default_dir() -> Result<PathBuf> {
    Ok(crate::paths::data_dir()?.join("export"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{Block, Run, RunStyle};

    /// Turns a chapter into Markdown without a title line of its own.
    fn chapter_markdown(chapter: &Chapter) -> String {
        chapter_markdown_titled(chapter, None, None)
    }

    fn block(kind: BlockKind, text: &str) -> Block {
        Block {
            kind,
            runs: vec![Run {
                text: text.into(),
                style: RunStyle::default(),
            }],
        }
    }

    fn chapter(blocks: Vec<Block>) -> Chapter {
        Chapter {
            href: "OEBPS/ch01.xhtml".into(),
            blocks,
            ..Chapter::default()
        }
    }

    #[test]
    fn one_title_line_names_book_and_chapter_and_books_headings_keep_their_levels() {
        // Exactly one first-level heading: the title, whether the chapter
        // opens with a heading of its own or with nothing but prose. The
        // book's own headings sit below it.
        let md = chapter_markdown_titled(
            &chapter(vec![
                block(BlockKind::Heading(1), "34"),
                block(BlockKind::Heading(1), "Pattern: Snapshots"),
            ]),
            Some("Understanding Eventsourcing"),
            Some("34. Pattern: Snapshots"),
        );
        assert!(
            md.starts_with("# Understanding Eventsourcing · 34. Pattern: Snapshots"),
            "{md}"
        );
        assert_eq!(
            md.lines().filter(|l| l.starts_with("# ")).count(),
            1,
            "{md}"
        );
        assert!(md.contains("## 34"));
        assert!(md.contains("## Pattern: Snapshots"));

        let md = chapter_markdown_titled(
            &chapter(vec![block(BlockKind::Paragraph, "just prose")]),
            Some("A Book"),
            Some("1. Beginning"),
        );
        assert!(md.starts_with("# A Book · 1. Beginning"), "{md}");

        // And without a title to add, nothing is shifted: the book's own
        // first heading stays where the book put it, and the levels under it
        // are the levels the book gave them.
        let md = chapter_markdown(&chapter(vec![block(BlockKind::Heading(1), "Title")]));
        assert!(md.contains("# Title"));
        assert!(!md.contains("## Title"));

        let md = chapter_markdown(&chapter(vec![
            block(BlockKind::Heading(1), "Title"),
            block(BlockKind::Heading(3), "Detail"),
        ]));
        assert!(md.contains("# Title"));
        assert!(md.contains("### Detail"));
    }

    #[test]
    fn code_is_fenced_once_per_run() {
        // One fence open, one close, not one pair per line — and a run with
        // no closing line of its own is closed at the end of the chapter.
        let md = chapter_markdown(&chapter(vec![
            block(BlockKind::Paragraph, "before"),
            block(BlockKind::Code, "line one"),
            block(BlockKind::Code, "line two"),
            block(BlockKind::Paragraph, "after"),
        ]));
        assert_eq!(md.matches("```").count(), 2, "{md}");
        assert!(md.contains("line one\nline two"));

        let md = chapter_markdown(&chapter(vec![block(BlockKind::Code, "last")]));
        assert_eq!(md.matches("```").count(), 2, "{md}");
    }

    #[test]
    fn lists_keep_their_shape() {
        let md = chapter_markdown(&chapter(vec![
            block(
                BlockKind::ListItem {
                    depth: 0,
                    ordinal: Some(1),
                },
                "first",
            ),
            block(
                BlockKind::ListItem {
                    depth: 1,
                    ordinal: None,
                },
                "nested",
            ),
        ]));
        assert!(md.contains("1. first"));
        assert!(md.contains("  - nested"));
    }

    #[test]
    fn an_exported_file_says_where_it_came_from() {
        // Front matter a program can read back: the book, the chapter and the
        // title — a colon in the title and all — and nothing at all where
        // there is no book to name, which is not an origin.
        let text = format!(
            "{}\nbody",
            front_matter(&[
                ("book", "sha256:abc"),
                ("title", "Kamal Handbook: The missing manual"),
            ])
        );
        let origin = parse_origin(&text).expect("front matter");
        assert_eq!(origin.book, "sha256:abc");

        let text = format!(
            "{}\n# Body\n",
            front_matter(&[
                ("book", "sha256:abc"),
                ("title", "A Book"),
                ("chapter", "OEBPS/ch02.xhtml"),
            ])
        );
        assert_eq!(
            parse_origin(&text),
            Some(Origin {
                book: "sha256:abc".into(),
                chapter: Some("OEBPS/ch02.xhtml".into()),
            })
        );

        assert!(parse_origin("# Just a heading\n").is_none());
        assert!(parse_origin("").is_none());
        // Front matter without a book reference is not an origin either.
        assert!(parse_origin("---\ntitle: \"x\"\n---\n").is_none());
    }

    #[test]
    fn slugs_are_safe_file_names() {
        assert_eq!(
            slugify("Kamal Handbook: The missing manual"),
            "kamal-handbook-the-missing-manual"
        );
        assert_eq!(slugify("C++ für Anfänger"), "c-f-r-anf-nger");
        assert_eq!(slugify("!!!"), "untitled");
        assert!(slugify(&"x".repeat(200)).chars().count() <= 60);
    }
}
