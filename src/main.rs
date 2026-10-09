//! omaread - a terminal ebook reader with library management.

mod app;
mod cli;
mod commands;
mod doc;
mod epub;
mod export;
mod find;
mod i18n;
mod identity;
mod image;
mod journal;
mod layout;
mod library;
mod measure;
mod paths;
mod pictures;
mod reference;
mod report;
mod search;
mod session;
mod shelf;
mod theme;
mod tty;
mod ui;

#[cfg(test)]
mod testapp;
#[cfg(test)]
mod testkit;

use anyhow::{Result, bail};
use clap::Parser;
use cli::{Cli, Command, Fields, JournalCommand};
use commands::{
    dump_chapter, edit_matched, export_library, forget, inspect, journal_compact, journal_status,
    list_authors, list_images, list_library, metadata,
};
use reference::{book_file, open_argument, read_anything, scan_directory};
use session::{browse, find_and_open, restore_sigpipe};
use std::path::Path;


fn main() -> Result<()> {
    restore_sigpipe();
    let cli = Cli::parse();
    check_invocation(&cli)?;

    if let Some(command) = &cli.command {
        return match command {
            Command::List { json, filter } => list_library(*json, filter.as_deref().unwrap_or("")),
            Command::Authors { json } => list_authors(*json),
            Command::Scan { dir, filenames } => scan_directory(dir, *filenames),
            Command::Find { text } => find_and_open(text),
            Command::Show { book, json } => metadata(book, &Fields::default(), *json),
            Command::Set { book, json, fields } => {
                metadata(book, &Fields::parse(fields)?, *json)
            }
            Command::Edit {
                filter,
                json,
                fields,
            } => edit_matched(filter, &Fields::parse(fields)?, *json),
            Command::Forget { what, json } => forget(what, *json),
            Command::Journal { command } => match command {
                JournalCommand::Status { json } => journal_status(*json),
                JournalCommand::Compact { json } => journal_compact(*json),
            },
            Command::Export {
                dir,
                force,
                reindex,
                embed,
            } => export_library(
                dir.as_ref()
                    .map(|path| path.to_string_lossy().into_owned())
                    .as_deref()
                    .unwrap_or(""),
                *force,
                *reindex,
                *embed,
            ),
            Command::Inspect { file } => inspect(open_argument(file)?),
            Command::Images { book } => list_images(book_file(Path::new(book))?),
            Command::Blocks { number, file } => dump_chapter(open_argument(file)?, *number),
        };
    }

    let Some(book) = cli.book.as_deref().filter(|book| !book.is_empty()) else {
        return browse();
    };
    read_anything(book, cli.chapter.as_deref(), cli.at.as_deref())
}

/// Checks what the command line asked for before anything runs.
///
/// A book, its flags and a subcommand are three ways of saying what to do now.
/// Two of them together used to run one and silently drop the other:
/// `omaread BOOK --chapter 3 list` printed the table and opened nothing.
fn check_invocation(cli: &Cli) -> Result<()> {
    let book = cli.book.as_deref().filter(|book| !book.is_empty());
    if cli.command.is_some() {
        if book.is_some() || cli.chapter.is_some() || cli.at.is_some() {
            bail!(
                "a book, --chapter and --at do not go with a subcommand: \
                 give one or the other"
            );
        }
    } else if book.is_none() && (cli.chapter.is_some() || cli.at.is_some()) {
        bail!("--chapter and --at are for a book: name one as well");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_book_and_a_subcommand_do_not_go_together() {
        // Both halves used to be taken and one of them printed: the table, and
        // no reader at the chapter that was asked for.
        for (book, chapter) in [
            (Some("某本书"), Some("3")),
            (Some("某本书"), None),
            (None, Some("3")),
        ] {
            let cli = Cli {
                book: book.map(str::to_string),
                chapter: chapter.map(str::to_string),
                at: None,
                command: Some(Command::List {
                    json: false,
                    filter: None,
                }),
            };
            let err = check_invocation(&cli).unwrap_err().to_string();
            assert!(err.contains("do not go with a subcommand"), "{err}");
        }
        // Neither on its own is exactly what was asked for.
        let cli = Cli {
            book: None,
            chapter: None,
            at: None,
            command: Some(Command::List {
                json: false,
                filter: None,
            }),
        };
        check_invocation(&cli).unwrap();

        // And a chapter flag with no book to read it in says so, rather than
        // opening nothing.
        let cli = Cli {
            book: None,
            chapter: Some("3".into()),
            at: None,
            command: None,
        };
        let err = check_invocation(&cli).unwrap_err().to_string();
        assert!(err.contains("for a book"), "{err}");
    }
}
