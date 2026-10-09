//! The lines every command prints.
//!
//! Commands read alike only while they share the lines they print with: an
//! absent field is a dash wherever it sits, a count of one has no `s`, and
//! `list --json` and `show --json` are the same object. Spelled once here, no
//! command can drift from the one printed next to it.

use crate::identity::BookId;
use crate::journal::{self, State};
use anyhow::Result;

/// The line above the table: how many books there are, and which word picked
/// them out. The count reads as a count — `1 book`, never `1 books`.
pub(crate) fn headline(count: usize, needle: &str) -> String {
    match needle.trim().is_empty() {
        true => counted(count, "book"),
        false => format!("{} matching {needle}", counted(count, "book")),
    }
}

/// A count and its noun, singular when the count is one.
pub(crate) fn counted(count: usize, noun: &str) -> String {
    match count {
        1 => format!("1 {noun}"),
        n => format!("{n} {noun}s"),
    }
}

/// The one way a value that is not there is shown: a dash.
///
/// A field nobody filled reads `-` wherever a value would sit — after a label
/// in `show` and `inspect`, in a column of `list` — so one absence looks the
/// same in every command rather than being left out, or explained in words of
/// its own.
pub(crate) fn or_dash(value: String) -> String {
    match value.trim().is_empty() {
        true => "-".to_string(),
        false => value,
    }
}

/// A path, or a value a book or a file holds, as it may be printed.
///
/// The terminal obeys what it is given: a file name carrying
/// `ESC ] 0 ; … BEL` retitles the window, and a title carrying U+202E flips
/// the rest of the line. `journal::clean` dismantles control characters and
/// drops format characters while keeping the text around them, and every
/// printed path and every printed field goes through it.
pub(crate) fn shown(value: impl std::fmt::Display) -> String {
    journal::clean(&value.to_string())
}

/// One field on a line of its own: the label, then the value cleaned on the
/// way out — what a book or a file says is shown, not obeyed, so one absence
/// reads the same in every command that prints a record.
///
/// The width is the label's column, which each report has always set for
/// itself: `show` and the file view line up at ten, `inspect` at eleven.
pub(crate) fn field(label: &str, value: String, width: usize) {
    println!("{label:<width$} {}", shown(value));
}

/// Prints a value as JSON, so every command that writes it writes it the same
/// way.
pub(crate) fn print_json<T: serde::Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// One book, by its id and its record: what `list --json` and `show --json`
/// both print.
pub(crate) fn record_json(id: &BookId, record: &journal::BookRecord, state: &State) -> serde_json::Value {
    serde_json::json!({
        "id": id.to_string(),
        "title": record.display_title(),
        "authors": record.authors,
        "series": record.series,
        "series_index": record.series_index,
        "tags": record.tags,
        "rating": record.rating,
        "publisher": record.publisher,
        "year": record.year,
        "language": record.language,
        "files": record.paths,
        "position": state.position(id).map(|at| serde_json::json!({
            "href": at.href,
            "block": at.block,
            "offset": at.offset,
        })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A journal holding one recorded position, as the reader would have
    /// left it, read back into the state the report answers from.
    fn a_journal_with_position(id: &BookId, block: usize) -> journal::State {
        let recorded = journal::Payload::PositionSet {
            href: "OEBPS/c1.xhtml".into(),
            block,
            offset: 2,
        };
        let dir = crate::journal::tests::journal_of("report-position", &[(id.clone(), recorded)]);
        journal::Journal::replay(&dir).unwrap()
    }

    #[test]
    fn the_line_above_the_table_counts_and_names_the_word() {
        assert_eq!(headline(1, "Oscar"), "1 book matching Oscar");
        assert_eq!(headline(2, "Oscar"), "2 books matching Oscar");
        assert_eq!(headline(1, ""), "1 book");
        assert_eq!(headline(12, "  "), "12 books");
    }

    #[test]
    fn an_absent_value_reads_as_a_dash() {
        assert_eq!(or_dash(String::new()), "-");
        assert_eq!(or_dash("   ".to_string()), "-");
        assert_eq!(or_dash("村上春树".to_string()), "村上春树");
    }

    #[test]
    fn the_json_carries_every_field_the_library_knows() {
        let mut record = journal::BookRecord::new(PathBuf::from("/books/a.epub"));
        record.title = Some("Anathem".into());
        record.authors = vec!["Stephenson".into()];
        record.series = Some("Norfolk".into());
        record.series_index = Some(1.0);
        record.tags = vec!["sf".into()];
        record.rating = Some(5);
        record.publisher = Some("Publisher".into());
        record.year = Some(2008);
        record.language = Some("en".into());
        let id = BookId::from(format!("sha256:{}", "ab".repeat(32)));

        let state = journal::State::default();
        let value = record_json(&id, &record, &state);
        for key in [
            "id",
            "title",
            "authors",
            "series",
            "series_index",
            "tags",
            "rating",
            "publisher",
            "year",
            "language",
            "files",
            "position",
        ] {
            assert!(value.get(key).is_some(), "the JSON has no {key}: {value}");
        }
        assert_eq!(value["series"], "Norfolk", "the table shows it too");
        assert_eq!(value["series_index"], 1.0);
        assert_eq!(value["files"][0], "/books/a.epub");
        assert!(value["position"].is_null(), "nothing recorded yet");
    }

    #[test]
    fn where_the_reader_stopped_belongs_in_the_json_too() {
        let id = BookId::from(format!("sha256:{}", "ab".repeat(32)));
        let record = journal::BookRecord::new(PathBuf::from("/books/a.epub"));
        let state = a_journal_with_position(&id, 4);

        let value = record_json(&id, &record, &state);
        assert_eq!(value["position"]["href"], "OEBPS/c1.xhtml");
        assert_eq!(value["position"]["block"], 4);
    }
}
