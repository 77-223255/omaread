//! The command line: the clap surface and the field values it parses.

use crate::journal::Payload;
use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "omaread",
    version,
    about = "Terminal ebook reader",
    after_help = "EXAMPLES:
  omaread                       read the library
  omaread 某本书                 read one of its books
  omaread list --filter 村上     what the library holds
  omaread show 某本书            what a book says about itself
  omaread set 某本书 title=新名   correct it, without touching the file
  omaread forget 某本书          take it out, so a scan reads it afresh"
)]
pub struct Cli {
    /// A book to read: its id, the name the library knows it by, or a file.
    /// Left out, the library opens
    #[arg(value_name = "BOOK")]
    pub book: Option<String>,

    /// With a book: the chapter to open, by its number in the spine (counting
    /// from 1) or by its href
    #[arg(long, value_name = "CHAPTER")]
    pub chapter: Option<String>,

    /// With a book: jump to the first occurrence of this text
    #[arg(long, value_name = "TEXT")]
    pub at: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// The library as a table
    List {
        /// Print JSON rather than a table, for a program
        #[arg(long)]
        json: bool,
        /// Only the books this word picks out: a title, an author, a series, a tag
        #[arg(long, value_name = "TEXT")]
        filter: Option<String>,
    },
    /// The distinct author names, and how many books each is on
    Authors {
        /// Print JSON rather than a table, for a program
        #[arg(long)]
        json: bool,
    },
    /// Add the books of a directory to the library, and notice moved ones
    Scan {
        /// The directory to look through, and everything below it
        #[arg(value_name = "DIR")]
        dir: PathBuf,
        /// Name each book from its file name rather than from inside the file
        #[arg(long)]
        filenames: bool,
    },
    /// Search the whole library and read the hit
    Find {
        /// The words to search for
        #[arg(value_name = "TEXT")]
        text: String,
    },
    /// What a book says about itself
    Show {
        /// A book id, a name the library knows, or a file
        #[arg(value_name = "BOOK")]
        book: String,
        /// Print JSON rather than aligned columns, for a program
        #[arg(long)]
        json: bool,
    },
    /// Correct what a book says about itself, without touching the file
    Set {
        /// A book id, or the name the library knows the book by
        #[arg(value_name = "BOOK")]
        book: String,
        /// Print the corrected book as JSON, for a program
        #[arg(long)]
        json: bool,
        /// title=… authors=a,b series=… series-index=… tags=a,b rating=1-5
        /// publisher=… year=… language=… An empty value clears the field
        #[arg(value_name = "FIELD=VALUE", required = true)]
        fields: Vec<String>,
    },
    /// Correct every book a word picks out, in one call
    Edit {
        /// The word that picks the books out: a title, an author, a series, a tag
        #[arg(value_name = "TEXT")]
        filter: String,
        /// Print the corrected books as JSON, for a program
        #[arg(long)]
        json: bool,
        /// title=… authors=a,b series=… series-index=… tags=a,b rating=1-5
        /// publisher=… year=… language=… An empty value clears the field
        #[arg(value_name = "FIELD=VALUE", required = true)]
        fields: Vec<String>,
    },
    /// Take a book, or a whole shelf, out of the library.
    ///
    /// A forgotten book is removed rather than hidden: the record and the
    /// reading position go with it, and what a scan reads in next starts the
    /// book over.
    Forget {
        /// A book id, a name, a file, or a directory of them
        #[arg(value_name = "BOOK")]
        what: String,
        /// Print the forgotten books as JSON, for a program
        #[arg(long)]
        json: bool,
    },
    /// The event log: what it holds, and how much of it still matters
    Journal {
        #[command(subcommand)]
        command: JournalCommand,
    },
    /// Write the library as Markdown, one file per chapter
    Export {
        /// Where to write them; left out, the data directory's export folder
        /// (~/.local/share/omaread/export)
        #[arg(value_name = "DIR")]
        dir: Option<PathBuf>,
        /// Export every book, even those unchanged since the last run
        #[arg(long)]
        force: bool,
        /// Have qmd re-index afterwards
        #[arg(long)]
        reindex: bool,
        /// With --reindex: also update the embeddings, which takes a while
        #[arg(long, requires = "reindex")]
        embed: bool,
    },
    /// What a book file holds, without the library
    Inspect {
        /// A book file, or a book the library knows by id or name
        #[arg(value_name = "BOOK")]
        file: String,
    },
    /// The pictures in a book file, and how big each one is
    Images {
        /// A book file, or a book the library knows by id or name
        #[arg(value_name = "BOOK")]
        book: String,
    },
    /// The parsed blocks of one chapter of a book file
    Blocks {
        /// The chapter's number in the spine, counting from 1
        #[arg(value_name = "N")]
        number: usize,
        /// A book file, or a book the library knows by id or name
        #[arg(value_name = "BOOK")]
        file: String,
    },
}

/// What `omaread journal` was asked to do with the log.
#[derive(Subcommand)]
pub enum JournalCommand {
    /// What the log holds and how big it is
    Status {
        /// Print JSON rather than a table, for a program
        #[arg(long)]
        json: bool,
    },
    /// Fold this machine's log to the events that still matter
    Compact {
        /// Print JSON rather than a line of prose, for a program
        #[arg(long)]
        json: bool,
    },
}

/// What `omaread set` was asked to change, in the words the command line used.
#[derive(Debug, Default)]
pub struct Fields {
    title: Option<String>,
    authors: Option<Vec<String>>,
    series: Option<String>,
    series_index: Option<f32>,
    tags: Option<Vec<String>>,
    rating: Option<u8>,
    publisher: Option<String>,
    year: Option<i32>,
    language: Option<String>,
}

impl Fields {
    /// Reads `field=value` pairs.
    ///
    /// An empty value clears the field, which is how a book goes back to what its
    /// own file says — so `title=` and `title=Something` are the two things a
    /// person needs, and there is no third flag for either.
    pub fn parse(pairs: &[String]) -> Result<Self> {
        let mut fields = Fields::default();
        for pair in pairs {
            let (name, value) = pair.split_once('=').with_context(|| {
                format!("{pair:?} is not a field: write it as title=… or authors=a,b")
            })?;
            // A title carrying an escape sequence would be obeyed by whatever
            // terminal prints it: a title can retitle a window or repaint the
            // screen. Nothing a person types here needs one.
            if value.chars().any(char::is_control) {
                bail!("{name} carries a control character; \t, \n and escapes are not text");
            }
            match name.trim() {
                "title" => fields.title = Some(value.to_string()),
                "authors" => fields.authors = Some(parse_list(value)?),
                "series" => fields.series = Some(value.to_string()),
                "series-index" => {
                    fields.series_index = Some(value.trim().parse().with_context(|| {
                        format!("{value:?} is not a number to sit at in a series")
                    })?)
                }
                "tags" => fields.tags = Some(parse_list(value)?),
                "rating" => {
                    let rating: u8 = value
                        .trim()
                        .parse()
                        .with_context(|| format!("{value:?} is not a rating from 1 to 5"))?;
                    if rating > 5 {
                        bail!("a rating goes from 1 to 5, not {rating}");
                    }
                    fields.rating = Some(rating);
                }
                "publisher" => fields.publisher = Some(value.to_string()),
                "year" => {
                    fields.year = Some(
                        value
                            .trim()
                            .parse()
                            .with_context(|| format!("{value:?} is not a year"))?,
                    )
                }
                "language" => fields.language = Some(value.to_string()),
                other => bail!(
                    "{other:?} is not something a book says about itself; there are \
                     title, authors, series, series-index, tags, rating, publisher, \
                     year and language"
                ),
            }
        }
        Ok(fields)
    }

    /// True when nothing was asked for, which makes `set` a `show`.
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.authors.is_none()
            && self.series.is_none()
            && self.series_index.is_none()
            && self.tags.is_none()
            && self.rating.is_none()
            && self.publisher.is_none()
            && self.year.is_none()
            && self.language.is_none()
    }

    /// What the journal records for this correction.
    pub fn changes(&self) -> Payload {
        Payload::MetadataSet {
            title: self.title.clone(),
            authors: self.authors.clone(),
            series: self.series.clone(),
            series_index: self.series_index,
            tags: self.tags.clone(),
            rating: self.rating,
            publisher: self.publisher.clone(),
            year: self.year,
            language: self.language.clone(),
        }
    }
}

/// Splits a comma-separated list, in either comma, and drops the empties.
pub(crate) fn split_list(text: &str) -> Vec<String> {
    text.split([',', '，'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

/// Reads a list value: a JSON array of strings, or a comma-separated list.
///
/// The JSON form is how a name holding a comma survives — `authors=["Le Guin,
/// Ursula"]` is one author, where the comma form would read it as two. Anything
/// else is the comma form, which is what a person types at a shell.
pub(crate) fn parse_list(value: &str) -> Result<Vec<String>> {
    let trimmed = value.trim();
    if trimmed.starts_with('[') {
        let items: Vec<String> = serde_json::from_str(trimmed)
            .with_context(|| format!("{trimmed:?} is not a JSON list of names"))?;
        return Ok(items
            .into_iter()
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect());
    }
    Ok(split_list(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_value_is_json_when_it_is_one_and_commas_otherwise() {
        // The comma form is what a person types, and an empty value clears.
        assert_eq!(parse_list("A, B ,C").unwrap(), ["A", "B", "C"]);
        assert_eq!(parse_list("A，B").unwrap(), ["A", "B"], "the wide comma too");
        assert_eq!(parse_list("").unwrap(), Vec::<String>::new());

        // The JSON form is how a name that holds a comma survives: it is one
        // author, where the comma form would read it as two.
        assert_eq!(parse_list(r#"["Le Guin, Ursula"]"#).unwrap(), ["Le Guin, Ursula"]);
        assert_eq!(parse_list(r#"[ "a" , "b" ]"#).unwrap(), ["a", "b"]);
        assert!(parse_list("[not json").is_err());
    }
}
