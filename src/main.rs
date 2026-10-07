//! omaread - a terminal ebook reader with library management.

mod app;
mod decision;
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
mod paths;
mod search;
mod shelf;
mod sorts;
mod theme;
mod ui;

use anyhow::{Context, Result, bail};
use app::App;
use clap::{Parser, Subcommand};
use epub::Book;
use identity::BookId;
use journal::{Journal, Payload, State};
use layout::LayoutOptions;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use std::io::Write;
use std::path::{Path, PathBuf};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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
struct Cli {
    /// A book to read: its id, the name the library knows it by, or a file.
    /// Left out, the library opens
    #[arg(value_name = "BOOK")]
    book: Option<String>,

    /// With a book: the chapter to open, by its number in the spine (counting
    /// from 1) or by its href
    #[arg(long, value_name = "CHAPTER")]
    chapter: Option<String>,

    /// With a book: jump to the first occurrence of this text
    #[arg(long, value_name = "TEXT")]
    at: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// The library as a table
    List {
        /// Print JSON rather than a table, for a program
        #[arg(long)]
        json: bool,
        /// Only the books this word picks out: a title, an author, a series, a tag
        #[arg(long, value_name = "TEXT")]
        filter: Option<String>,
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

/// What `omaread set` was asked to change, in the words the command line used.
#[derive(Debug, Default)]
struct Fields {
    title: Option<String>,
    authors: Option<String>,
    series: Option<String>,
    series_index: Option<f32>,
    tags: Option<String>,
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
    fn parse(pairs: &[String]) -> Result<Self> {
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
                "authors" => fields.authors = Some(value.to_string()),
                "series" => fields.series = Some(value.to_string()),
                "series-index" => {
                    fields.series_index = Some(value.trim().parse().with_context(|| {
                        format!("{value:?} is not a number to sit at in a series")
                    })?)
                }
                "tags" => fields.tags = Some(value.to_string()),
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
    fn is_empty(&self) -> bool {
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
}

fn main() -> Result<()> {
    restore_sigpipe();
    let cli = Cli::parse();
    check_invocation(&cli)?;

    if let Some(command) = &cli.command {
        return match command {
            Command::List { json, filter } => list_library(*json, filter.as_deref().unwrap_or("")),
            Command::Scan { dir, filenames } => scan_directory(dir, *filenames),
            Command::Find { text } => find_and_open(text),
            Command::Show { book, json } => {
                let config = paths::Config::load()?;
                metadata(book, &config.journal_dir()?, &Fields::default(), *json)
            }
            Command::Set { book, fields } => {
                let config = paths::Config::load()?;
                metadata(book, &config.journal_dir()?, &Fields::parse(fields)?, false)
            }
            Command::Forget { what } => {
                let config = paths::Config::load()?;
                forget(what, &config.journal_dir()?)
            }
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

/// Opens the book an argument names: a file, or a book the library knows.
///
/// `inspect`, `images` and `blocks` used to take only a path and `show` only a
/// book the library holds, so one ordinary question could need two commands.
/// What each of them prints is unchanged; only what may be passed is wider.
fn open_argument(reference: &str) -> Result<Book> {
    let path = PathBuf::from(shellexpand(reference));
    if path.is_file() {
        return book_file(&path);
    }
    book_file(&library_file(reference)?)
}

/// The file a book the library knows lives in.
fn library_file(reference: &str) -> Result<PathBuf> {
    let config = paths::Config::load()?;
    let state = Journal::replay(&config.journal_dir()?)?;
    let id = library::resolve(&state, reference)?;
    let record = state
        .book(&id)
        .with_context(|| format!("the journal knows nothing about {id}"))?;
    record
        .path()
        .cloned()
        .with_context(|| format!("no file recorded for {}", record.display_title()))
}

/// Opens a book file, saying which file when it cannot be read.
fn book_file(path: &Path) -> Result<Book> {
    Book::open(path).with_context(|| format!("cannot read {}", shown(path.display())))
}

/// Reads whatever a name points at.
///
/// A file on disk is a book file, and reading it adds it to the library as it
/// goes. Anything else is a book the library knows — by id, or by the words on
/// its spine — or a file `export` wrote, and `open_reference` sorts those out.
fn read_anything(reference: &str, chapter: Option<&str>, at: Option<&str>) -> Result<()> {
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

/// Takes books out of the library, and everything recorded about them with it.
///
/// The events stay in the journal — it only ever grows — but replaying it no
/// longer finds the book: no record, no reading position. What a scan adds next
/// is a book nobody has read.
///
/// What picks the books is a path when the path exists — that is how a person
/// points at a shelf, and a folder means everything under it — and a book id or a
/// title otherwise.
fn forget(reference: &str, journal_dir: &Path) -> Result<()> {
    let state = Journal::replay(journal_dir)?;

    let as_path = PathBuf::from(shellexpand(reference));
    if as_path.exists() {
        return forget_under(&as_path, &state, journal_dir);
    }

    let id = library::resolve(&state, reference)?;
    let title = state
        .book(&id)
        .map(|record| record.display_title())
        .unwrap_or_default();

    let mut journal = Journal::open(journal_dir)?;
    journal.append(&id, Payload::BookForgotten)?;
    println!("forgot {title}");
    Ok(())
}

/// Forgets every book the library holds under a directory, or the one file that
/// was named.
fn forget_under(root: &Path, state: &State, journal_dir: &Path) -> Result<()> {
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
fn metadata(reference: &str, journal_dir: &Path, fields: &Fields, json: bool) -> Result<()> {
    let state = Journal::replay(journal_dir)?;

    // A path names a book as an id or a title does. The library's record is
    // the answer when it has seen the file — corrections and all — and what
    // the file itself says when it never has.
    let path = PathBuf::from(shellexpand(reference));
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
            return known_book(&state, journal_dir, &id, fields, json);
        }
        let id = BookId::of_file(&path)?;
        if state.book(&id).is_some() {
            return known_book(&state, journal_dir, &id, fields, json);
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
    known_book(&state, journal_dir, &id, fields, json)
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
    let changes = Payload::MetadataSet {
        title: fields.title.clone(),
        authors: fields.authors.as_deref().map(split_list),
        series: fields.series.clone(),
        series_index: fields.series_index,
        tags: fields.tags.as_deref().map(split_list),
        rating: fields.rating,
        publisher: fields.publisher.clone(),
        year: fields.year,
        language: fields.language.clone(),
    };

    if fields.is_empty() {
        let record = state
            .book(id)
            .with_context(|| format!("the journal knows nothing about {id}"))?;
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&record_json(id, record, state))?
            );
            return Ok(());
        }
        // Every field on a line of its own, and a value that is not there
        // reads `-`: one rule, so one absence looks the same as in `list` and
        // `inspect`. Every value is cleaned on the way out: what a book says
        // is shown, not obeyed.
        let line = |label: &str, value: String| println!("{label:<10} {}", journal::clean(&value));
        line("title:", or_dash(record.title.clone().unwrap_or_default()));
        line("authors:", record.display_authors());
        line(
            "series:",
            or_dash(match (&record.series, record.series_index) {
                (Some(name), Some(at)) => format!("{name} {at}"),
                (Some(name), None) => name.clone(),
                _ => String::new(),
            }),
        );
        line("tags:", or_dash(record.tags.join(", ")));
        line(
            "rating:",
            or_dash(record.rating.map(|r| r.to_string()).unwrap_or_default()),
        );
        line(
            "publisher:",
            or_dash(record.publisher.clone().unwrap_or_default()),
        );
        line(
            "year:",
            or_dash(record.year.map(|y| y.to_string()).unwrap_or_default()),
        );
        line(
            "language:",
            or_dash(record.language.clone().unwrap_or_default()),
        );
        line(
            "file:",
            or_dash(
                record
                    .path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            ),
        );
        line("id:", id.to_string());
        return Ok(());
    }

    let mut journal = Journal::open(journal_dir)?;
    journal.append(id, changes)?;
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
        println!(
            "{}",
            serde_json::to_string_pretty(&record_json(id, &record, &State::default()))?
        );
        return Ok(());
    }
    // Every value cleaned on the way out: a file's own title is shown, not
    // obeyed.
    let line = |label: &str, value: String| println!("{label:<10} {}", journal::clean(&value));
    // What a file does not hold reads the same dash as a field nobody filled
    // in the library.
    let absent = || "-".to_string();
    line("title:", or_dash(book.metadata.title.unwrap_or_default()));
    line("authors:", or_dash(book.metadata.authors.join(", ")));
    line("series:", absent());
    line("tags:", absent());
    line("rating:", absent());
    line("publisher:", absent());
    line("year:", absent());
    line(
        "language:",
        or_dash(book.metadata.language.unwrap_or_default()),
    );
    line("file:", path.display().to_string());
    line("id:", id.to_string());
    Ok(())
}

/// Splits a comma-separated list, in either comma, and drops the empties.
fn split_list(text: &str) -> Vec<String> {
    text.split([',', '，'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

/// Restores the usual reaction to a pipe nobody reads any more.
///
/// Rust ignores SIGPIPE, so a write into such a pipe returns an error and
/// `println!` turns that into a panic: `omaread list | head` ended in a
/// backtrace instead of simply stopping. The default action ends the process
/// quietly, which is how every other command behaves in a pipeline.
fn restore_sigpipe() {
    // Sound here and nowhere later: no other thread runs yet, so nothing can
    // observe the disposition while it changes.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
}

/// Prepares what a fresh installation needs before the first screen appears.
///
/// The reader is made for Omarchy, so following the theme is not an extra step
/// somebody has to find in a README: the template goes in on its own and the
/// first session already has the right colours. Both parts leave an existing
/// installation untouched.
fn first_start() -> Result<()> {
    paths::Config::write_default_if_missing()?;
    if theme::install_template() {
        println!("Hooked omaread into your Omarchy theme.");
    }
    Ok(())
}

/// Set once this program has handed itself to a terminal, so a terminal that
/// fails to start it cannot start itself again for ever.
const HANDED_OVER: &str = "OMAREAD_IN_TERMINAL";

/// Makes sure there is a terminal to draw on, by asking for one when there is
/// not.
///
/// An app launcher, a file manager or a window manager starts a program with no
/// terminal of its own, and this program draws on one. Without this, opening it
/// from a menu is a window that flashes and is gone, which says nothing; with
/// it, the library opens in a terminal and a book is one keypress away.
///
/// Only the paths that draw ask for this. A command that answers on stdout — the
/// library as JSON, a scan, an export — must never be handed to
/// a terminal, because what asked for it is a pipe, not a person.
fn ensure_terminal() -> Result<()> {
    use std::io::IsTerminal;

    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        return Ok(());
    }
    if std::env::var_os(HANDED_OVER).is_some() {
        bail!("no terminal to draw on, and the one this was handed to did not start it");
    }

    // This program by path rather than by name: the copy that was clicked has to
    // be the copy that runs, whatever `omaread` happens to mean on `PATH`.
    let itself = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("omaread"));
    let status = std::process::Command::new("xdg-terminal-exec")
        .arg(itself)
        .args(std::env::args_os().skip(1))
        .env(HANDED_OVER, "1")
        .status();

    match status {
        // The terminal has the child now; this process has nothing left to do.
        // Its exit status is the terminal's, not the reader's, so anything that
        // ran at all counts as handed over.
        Ok(status) if status.success() => std::process::exit(0),
        Ok(status) => bail!("the terminal exited with {status}"),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => bail!(
            "this draws on a terminal: run it from one, or install xdg-terminal-exec \
             so it can open a terminal of its own"
        ),
        Err(err) => Err(err).context("cannot start a terminal"),
    }
}

/// Opens the library and reads whichever book is picked, until the reader quits.
///
/// Shelf and reader are separate screens with one terminal between them. The
/// journal is replayed on every return, so a reading position made just now
/// shows on the shelf straight away.
fn browse() -> Result<()> {
    ensure_terminal()?;
    first_start()?;
    let config = paths::Config::load()?;
    let journal_dir = config.journal_dir()?;

    let backend = choose_image_backend(config.images.as_deref())?;
    let mut terminal = ratatui::init();
    let cell = cell_size(&terminal);
    let mut theme = theme::Watcher::new();
    // One client for the whole session, so its cache of answers survives the
    // shelf being rebuilt every time a book is left.
    let mut decision = decision::Decision::new(
        decision::Config::resolve(
            config.decision.base_url.clone(),
            config.decision.model.clone(),
            config.decision.api_key_env.clone(),
        ),
        decision::Curl,
    );

    let result = (|| -> Result<()> {
        loop {
            let state = Journal::replay(&journal_dir)?;
            let mut shelf = shelf::Shelf::new(&state);
            shelf.set_decision_ready(decision.available());
            if shelf.total() == 0 {
                ratatui::restore();
                empty_library_hint();
                return Ok(());
            }

            // The shelf runs until a book is picked or the reader quits.
            let picked = loop {
                theme.refresh();
                let colours = theme.theme();
                terminal.draw(|frame| ui::draw_shelf(frame, &mut shelf, &colours))?;
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        match shelf.handle_key(key) {
                            shelf::Action::Open { id, path } => break Some((id, path)),
                            shelf::Action::Quit => break None,
                            shelf::Action::None => {}
                        }
                        // An order that has to be asked of the model: show the
                        // waiting line, then go and ask, so the screen is not
                        // blank while the request is out.
                        if shelf.asking() {
                            terminal.draw(|frame| ui::draw_shelf(frame, &mut shelf, &colours))?;
                            shelf.resolve(&mut decision);
                        }
                    }
                    _ => {}
                }
            };

            let Some((id, path)) = picked else {
                return Ok(());
            };
            let book = match Book::open(&path) {
                Ok(book) => book,
                Err(_) => continue,
            };

            let mut journal = Journal::open(&journal_dir)?;
            journal.assume_written(state.position(&id).cloned());
            let options = LayoutOptions {
                max_width: config.max_width.unwrap_or(u16::MAX),
            };
            let mut app = App::new(book, id, journal, &state, options)?;
            app.set_image_backend(backend, cell);

            repaint_everything(&mut terminal)?;
            event_loop(&mut terminal, &mut app)?;
            app.save_position();

            // Back to the shelf, with a clean screen: the reader may have left
            // pixels behind that no cell redraw would remove.
            repaint_everything(&mut terminal)?;
            if app.should_quit_program {
                return Ok(());
            }
        }
    })();

    ratatui::restore();
    result
}

/// Searches the library and opens whichever hit is picked.
///
/// The hits are gathered before the terminal is taken over, so progress from a
/// direct search is visible and a slow run can be interrupted.
fn find_and_open(query: &str) -> Result<()> {
    ensure_terminal()?;
    let config = paths::Config::load()?;
    let journal_dir = config.journal_dir()?;
    let state = Journal::replay(&journal_dir)?;

    let mut last = std::time::Instant::now();
    let results = find::find(query, &state, 40, &mut |title| {
        // Only every so often: a line per book would scroll the report away.
        if last.elapsed() > std::time::Duration::from_millis(400) {
            last = std::time::Instant::now();
            println!("  searching {title} ...");
        }
    })?;

    if results.hits.is_empty() {
        println!("{}", library::no_match(query));
        if matches!(results.source, find::Source::Direct) {
            println!("\nFor faster and broader search, index the library:");
            println!("  omaread export --reindex");
        }
        return Ok(());
    }

    // Pick a hit, then open the book there.
    let backend = choose_image_backend(config.images.as_deref())?;
    let mut terminal = ratatui::init();
    let cell = cell_size(&terminal);
    let mut theme = theme::Watcher::new();
    let picked = pick_hit(&mut terminal, &results, &mut theme);
    let outcome = (|| -> Result<()> {
        let Some(index) = picked? else { return Ok(()) };
        let hit = &results.hits[index];
        let path = find::file_of(hit, &state)?;
        let book =
            Book::open(&path).with_context(|| format!("cannot read {}", shown(path.display())))?;

        let id = hit.book.clone();
        let mut journal = Journal::open(&journal_dir)?;
        journal.assume_written(state.position(&id).cloned());
        let options = LayoutOptions {
            max_width: config.max_width.unwrap_or(u16::MAX),
        };
        let mut app = App::new(book, id, journal, &state, options)?;
        app.set_image_backend(backend, cell);
        if let Some(href) = &hit.chapter_href {
            app.go_to_href(href);
        }
        // The passage, so the reader lands on the sentence rather than the chapter.
        app.search_for(hit.passage.clone().unwrap_or_else(|| query.to_string()));

        repaint_everything(&mut terminal)?;
        event_loop(&mut terminal, &mut app)?;
        app.save_position();
        Ok(())
    })();

    ratatui::restore();
    outcome
}

/// Shows the hits and returns the chosen one.
fn pick_hit(
    terminal: &mut ratatui::DefaultTerminal,
    results: &find::Results,
    theme: &mut theme::Watcher,
) -> Result<Option<usize>> {
    let mut cursor = 0usize;
    let mut scroll = 0usize;
    let last = results.hits.len().saturating_sub(1);

    loop {
        theme.refresh();
        let colours = theme.theme();
        // Three rows per hit, so the visible count follows the window height.
        let per_screen = ((terminal.size()?.height.saturating_sub(1)) / 3).max(1) as usize;
        if cursor < scroll {
            scroll = cursor;
        } else if cursor >= scroll + per_screen {
            scroll = cursor + 1 - per_screen;
        }
        terminal.draw(|frame| ui::draw_hits(frame, results, cursor, scroll, &colours))?;

        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                ratatui::crossterm::event::KeyCode::Char('q')
                | ratatui::crossterm::event::KeyCode::Esc => return Ok(None),
                ratatui::crossterm::event::KeyCode::Char('j')
                | ratatui::crossterm::event::KeyCode::Down => cursor = (cursor + 1).min(last),
                ratatui::crossterm::event::KeyCode::Char('k')
                | ratatui::crossterm::event::KeyCode::Up => cursor = cursor.saturating_sub(1),
                ratatui::crossterm::event::KeyCode::Char('G') => cursor = last,
                ratatui::crossterm::event::KeyCode::Char('g') => cursor = 0,
                ratatui::crossterm::event::KeyCode::Enter
                | ratatui::crossterm::event::KeyCode::Char('l') => return Ok(Some(cursor)),
                _ => {}
            },
            _ => {}
        }
    }
}

/// Writes the library out as Markdown.
fn export_library(dir: &str, force: bool, reindex: bool, embed: bool) -> Result<()> {
    let config = paths::Config::load()?;
    let journal_dir = config.journal_dir()?;
    let state = Journal::replay(&journal_dir)?;
    let dir = if dir.is_empty() {
        export::default_dir()?
    } else {
        PathBuf::from(shellexpand(dir))
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
fn empty_library_hint() {
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

/// Opens a book at a place named by an exported file or a book id.
///
/// This is the way back from a search hit: the hit names a file, the file names
/// the book and chapter, and `--at` puts the reader on the passage rather than at
/// the top of a chapter.
fn open_reference(reference: &str, chapter: Option<&str>, at: Option<&str>) -> Result<()> {
    let config = paths::Config::load()?;
    let journal_dir = config.journal_dir()?;
    let state = Journal::replay(&journal_dir)?;

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
    let record = state
        .book(&id)
        .with_context(|| format!("no book with id {} in the library", id))?;
    let path = record
        .path()
        .cloned()
        .with_context(|| format!("no file recorded for {}", record.display_title()))?;

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
    std::path::Path::new(&shellexpand(file)).exists()
}

/// Where a file `export` wrote sends the reader: the book it came from, the
/// chapter, and the passage the reference names, if it names one.
fn origin_of_reference(reference: &str) -> Result<(String, Option<String>, Option<String>)> {
    let (path, line) = resolve_hit(reference)?;
    let origin = export::origin_of(&path)?;
    // A search hit names a line. Its text is the passage that matched, so it
    // makes a far better landing point than the top of a chapter.
    let text = line.and_then(|line| line_text(&path, line));
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
    Ok((PathBuf::from(shellexpand(body)), line))
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

/// The text of one line of a file, trimmed to something worth searching for.
///
/// A whole line can be a long paragraph; the first words are enough to find the
/// passage and are less likely to differ from the book by a stray character.
fn line_text(path: &Path, line: usize) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let raw = text.lines().nth(line.saturating_sub(1))?.trim();
    if raw.is_empty() || raw.starts_with("---") {
        return None;
    }
    // Markdown decoration would not appear in the book's own text.
    let cleaned = raw.trim_start_matches(['#', '>', '-', '*', ' ']).trim();
    let words: Vec<&str> = cleaned.split_whitespace().take(8).collect();
    if words.is_empty() {
        None
    } else {
        Some(words.join(" "))
    }
}

/// Expands a leading `~`, which a shell would otherwise have done.
fn shellexpand(path: &str) -> String {
    // `~` alone is the home directory too. Without this, a bare `~` was not a
    // path at all: it fell through to the library and matched whichever book
    // happened to have a `~` in its title.
    let rest = match path {
        "~" => "",
        other => match other.strip_prefix("~/") {
            Some(rest) => rest,
            None => return other.to_string(),
        },
    };
    match dirs::home_dir() {
        Some(home) => home.join(rest).to_string_lossy().into_owned(),
        None => path.to_string(),
    }
}

/// Reads a directory into the library.
fn scan_directory(dir: &Path, filenames: bool) -> Result<()> {
    // A path that is not a directory is a typo half the time, and `0 files seen`
    // with a zero exit is how it went unnoticed.
    if !dir.is_dir() {
        bail!("{} is not a directory to scan", shown(dir.display()));
    }
    let config = paths::Config::load()?;
    let journal_dir = config.journal_dir()?;
    let state = Journal::replay(&journal_dir)?;
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

/// Prints the library.
fn list_library(as_json: bool, needle: &str) -> Result<()> {
    let config = paths::Config::load()?;
    let state = Journal::replay(&config.journal_dir()?)?;
    let mut entries = library::entries(&state);
    library::sort(&mut entries, library::Order::Title, &library::Keys::default());
    let entries = library::filter(&entries, needle);

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
        let series = match (&record.series, record.series_index) {
            (Some(name), Some(index)) => format!("  [{name} {index}]"),
            (Some(name), None) => format!("  [{name}]"),
            _ => "  -".to_string(),
        };
        println!(
            "  {} {}{series}",
            fit(&record.display_title(), 34),
            fit(&record.display_authors(), 26)
        );
    }
    Ok(())
}

/// The line above the table: how many books there are, and which word picked
/// them out. The count reads as a count — `1 book`, never `1 books`.
fn headline(count: usize, needle: &str) -> String {
    match needle.trim().is_empty() {
        true => counted(count, "book"),
        false => format!("{} matching {needle}", counted(count, "book")),
    }
}

/// A count and its noun, singular when the count is one.
fn counted(count: usize, noun: &str) -> String {
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
fn or_dash(value: String) -> String {
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
fn shown(value: impl std::fmt::Display) -> String {
    journal::clean(&value.to_string())
}

/// Writes books as JSON, one object each, for a program to read.
fn print_books(entries: &[library::Entry], state: &State) -> Result<()> {
    let books: Vec<serde_json::Value> = entries
        .iter()
        .map(|entry| book_json(entry, state))
        .collect();

    println!("{}", serde_json::to_string_pretty(&books)?);
    Ok(())
}

/// One book as the library holds it: everything it knows, which is the table
/// plus what a table has no room for — the series it prints, the files it sits
/// in, and where the reader stopped.
///
/// An absent value is `null`, JSON's own dash. The same object `show --json`
/// prints, so a program reads one shape wherever it asks.
fn book_json(entry: &library::Entry, state: &State) -> serde_json::Value {
    record_json(&entry.id, &entry.record, state)
}

/// One book, by its id and its record: what `list --json` and `show --json`
/// both print.
fn record_json(id: &BookId, record: &journal::BookRecord, state: &State) -> serde_json::Value {
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

/// Opens a book, optionally at a chapter and a passage.
fn run_at(book: Book, path: PathBuf, chapter: Option<String>, at: Option<String>) -> Result<()> {
    // Before the terminal is taken over: a chapter that does not exist is an
    // error to read on an ordinary screen, not a status line in a reader that
    // has already swallowed the terminal.
    let target = chapter.map(|spec| chapter_of(&book, &spec)).transpose()?;
    ensure_terminal()?;
    first_start()?;
    let config = paths::Config::load()?;
    let journal_dir = config.journal_dir()?;

    let id = BookId::of_file(&path)?;
    let state = Journal::replay(&journal_dir)?;
    let restore = state.position(&id).cloned();
    let mut journal = Journal::open(&journal_dir)?;
    journal.assume_written(restore.clone());

    // Record the book so a journal read elsewhere can name it without opening
    // the file. Only when something changed, otherwise every start would add a
    // line that says nothing new.
    let here = path.canonicalize().unwrap_or_else(|_| path.clone());
    let known = state.book(&id);
    let unchanged = known.is_some_and(|record| record.paths.contains(&here));
    if !unchanged {
        // The file's own name is sent only when the library is hearing about this
        // book for the first time. Sending it again for a book already known
        // would fill a name that was cleared: `set BOOK title=` says "use the file's
        // own name", and the file would put its `dc:title` back the next time the
        // book was opened — a correction undone by reading.
        let first_time = known.is_none();
        journal.append(
            &id,
            journal::Payload::BookSeen {
                title: if first_time {
                    book.metadata.title.clone()
                } else {
                    None
                },
                authors: if first_time {
                    book.metadata.authors.clone()
                } else {
                    Vec::new()
                },
                path: here,
            },
        )?;
    }

    // Replay once more when the book was just written. The reader shows the name
    // the library knows, and the library knows it only now that `BookSeen` is in
    // the journal: otherwise a book opened straight from its path would be
    // called "Untitled" at the foot of the screen while the shelf, which reads
    // the journal, calls it what its file is called.
    let state = if unchanged {
        state
    } else {
        Journal::replay(&journal_dir)?
    };

    let options = LayoutOptions {
        max_width: config.max_width.unwrap_or(u16::MAX),
    };
    let mut app = App::new(book, id, journal, &state, options)?;
    match target {
        Some(Chapter::Number(number)) => app.go_to_chapter_number(number),
        Some(Chapter::Href(href)) => app.go_to_href(&href),
        None => {}
    }
    if let Some(text) = at {
        app.search_for(text);
    }

    // Ask the terminal before the alternate screen is up, so its answer cannot
    // be mistaken for user input later on.
    let backend = choose_image_backend(config.images.as_deref())?;
    let mut terminal = ratatui::init();
    app.set_image_backend(backend, cell_size(&terminal));
    let result = event_loop(&mut terminal, &mut app);
    ratatui::restore();

    // Saving after restoring the terminal keeps an error message visible.
    app.save_position();
    result
}

fn event_loop(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<()> {
    let mut last_token = None;
    // Whether the previous frame put pixels on screen. Asking the app instead
    // would come too late: on a chapter change the old chapter's pictures are
    // already gone and the new one's are not rendered yet.
    let mut pixels_on_screen = false;

    while !app.should_leave_book {
        // Pixel pictures are not part of the cell buffer, so a diffed redraw
        // leaves them wherever no character happens to overwrite them. Once the
        // view has moved, the screen has to be wiped and painted afresh.
        let token = app.frame_token();
        let moved = last_token.is_some_and(|last| last != token);
        // Either direction matters: pictures that are on screen have to go, and
        // pictures that are about to appear need a clean surface.
        let wiping = moved && (pixels_on_screen || app.has_pixel_images());
        last_token = Some(token);

        {
            // The wipe above and the painting below are one frame to the reader,
            // so the terminal is told to hold the display until both are done.
            let _frame = HeldDisplay::begin();
            if wiping {
                repaint_everything(terminal)?;
            }
            // A theme switch replaces the colours under us. Checking after each
            // key is enough and costs one stat call.
            if app.refresh_theme() {
                repaint_everything(terminal)?;
            }

            let mut placements = Vec::new();
            terminal.draw(|frame| placements = ui::draw(frame, app))?;
            place_images(app, &placements)?;
            pixels_on_screen = !placements.is_empty();
        }

        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                app.handle_key(key);
                // A held-down key delivers keys faster than a chapter full of
                // pictures can be painted. Drawing each one would wipe the
                // screen that often, so what is already waiting is taken now
                // and shown as one frame.
                while !app.should_leave_book && event::poll(std::time::Duration::ZERO)? {
                    match event::read()? {
                        Event::Key(next) if next.kind == KeyEventKind::Press => {
                            app.handle_key(next)
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Keeps the terminal from showing a half-built frame.
///
/// A Sixel picture is part of the screen contents, so moving the view means
/// wiping the screen and painting it again. Without this the empty screen
/// between the two is visible, and scrolling past pictures flickers.
///
/// The mode is DEC 2026, which foot, Ghostty, kitty and others implement. A
/// terminal that does not know it ignores it, as it must for any private mode
/// it does not implement, so there is nothing to detect first.
struct HeldDisplay;

impl HeldDisplay {
    fn begin() -> Self {
        let mut out = std::io::stdout();
        // Failing to hold the display costs a flicker, not correctness, so a
        // write error here is not worth failing the frame over.
        let _ = out.write_all(b"\x1b[?2026h");
        let _ = out.flush();
        HeldDisplay
    }
}

impl Drop for HeldDisplay {
    fn drop(&mut self) {
        // Runs however the frame ended, so an error cannot leave the display
        // frozen.
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x1b[?2026l");
        let _ = out.flush();
    }
}

/// Wipes the screen and marks every cell as changed, so the next draw paints
/// everything. Avoids `Terminal::clear`, which asks the terminal for its cursor
/// position and waits for an answer.
fn repaint_everything(terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
    use ratatui::backend::Backend;
    let size = terminal.size().context("cannot read the terminal size")?;
    terminal.backend_mut().clear().context("cannot clear")?;
    terminal
        .resize(ratatui::layout::Rect::new(0, 0, size.width, size.height))
        .context("cannot reset the buffers")?;
    Ok(())
}

/// Decides how pictures are drawn: a setting wins, otherwise the terminal is
/// asked.
///
/// Asking needs raw mode, because the answer arrives as an escape sequence on
/// stdin. Raw mode is switched off again straight away, so a terminal that stays
/// silent leaves nothing behind.
fn choose_image_backend(setting: Option<&str>) -> Result<image::Backend> {
    if let Some(setting) = setting {
        if let Some(backend) = image::Backend::parse(setting) {
            return Ok(backend);
        }
        // A misspelling must not be read as "no pictures".
        eprintln!("omaread: unknown images setting {setting:?}, asking the terminal instead");
    }

    use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode};
    enable_raw_mode().context("cannot enter raw mode to query the terminal")?;
    let backend = image::detect::detect();
    disable_raw_mode().ok();
    Ok(backend)
}

/// Pixel size of one cell, as reported by the terminal. Falls back to a common
/// default when the terminal does not say.
fn cell_size(terminal: &ratatui::DefaultTerminal) -> image::CellSize {
    use ratatui::crossterm::terminal::window_size;
    let _ = terminal;
    match window_size() {
        Ok(size) if size.width > 0 && size.height > 0 && size.columns > 0 && size.rows > 0 => {
            image::CellSize {
                width: size.width / size.columns,
                height: size.height / size.rows,
            }
        }
        _ => image::CellSize::default(),
    }
}

/// Writes the pictures of the current frame.
///
/// Pixel protocols paint outside the text buffer, so ratatui knows nothing about
/// them. Old pictures are removed first, where the protocol allows addressing
/// them, and each remaining one is placed at the cursor position its reserved
/// lines start at.
fn place_images(app: &App, placements: &[ui::Placement]) -> Result<()> {
    let backend = app.image_backend();
    let clear = image::clear_all(backend);
    if clear.is_none() && placements.is_empty() {
        return Ok(());
    }

    use ratatui::crossterm::cursor::{MoveTo, RestorePosition, SavePosition};
    use ratatui::crossterm::queue;
    let mut out = std::io::stdout();

    if let Some(clear) = clear {
        queue!(out, ratatui::crossterm::style::Print(clear))?;
    }
    for placement in placements {
        queue!(
            out,
            SavePosition,
            MoveTo(placement.column, placement.row),
            ratatui::crossterm::style::Print(&placement.escape),
            RestorePosition
        )?;
    }
    out.flush()?;
    Ok(())
}

/// Prints one chapter's blocks with their kind, to see how markup came through.
fn dump_chapter(mut book: Book, number: usize) -> Result<()> {
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
            truncate(&text, 90)
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
fn list_images(mut book: Book) -> Result<()> {
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
            let Ok((width, height)) = image::dimensions(&bytes) else {
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
fn inspect(mut book: Book) -> Result<()> {
    // Every value cleaned on the way out — a book's own fields and paths are
    // shown, not obeyed, the same way `show` prints them — and an absent one
    // reads `-`, whatever the book left out.
    let line = |label: &str, value: String| println!("{label:<12}{}", shown(or_dash(value)));
    line("title:", book.metadata.title.clone().unwrap_or_default());
    line("authors:", book.metadata.authors.join(", "));
    line(
        "language:",
        book.metadata.language.clone().unwrap_or_default(),
    );
    line(
        "identifier:",
        book.metadata.identifier.clone().unwrap_or_default(),
    );
    // What was found for pictures, so a cover that draws small can be traced
    // to a book that never named one rather than to the drawing. A book that
    // declared no layout reads `-`: reflowable is the default, and printing
    // it as a declaration claimed more than the book said.
    line("layout:", book.layout.clone().unwrap_or_default());
    line("cover:", book.cover.clone().unwrap_or_default());
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
                    truncate(&shown(item.title.as_deref().unwrap_or(&item.href)), 48),
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
enum Chapter {
    /// The number as typed, counted from 1.
    Number(usize),
    Href(String),
}

/// Reads what `--chapter` says against the book it is for.
///
/// A number is the chapter's place in the spine — the same number `blocks`
/// takes — so `--chapter 3` opens the third chapter instead of hunting for an
/// href written "3". Anything else is an href, as it always was.
fn chapter_of(book: &Book, spec: &str) -> Result<Chapter> {
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

fn truncate(text: &str, width: usize) -> String {
    if UnicodeWidthStr::width(text) <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out + "…"
}

/// The text cut to a number of cells and then filled out to hold exactly that
/// many. Filling has to be counted in cells: `{:<width$}` counts characters,
/// which lines a column of Chinese titles up two cells short of every one of
/// them.
fn fit(text: &str, width: usize) -> String {
    let text = truncate(text, width);
    let used = UnicodeWidthStr::width(text.as_str());
    format!("{text}{}", " ".repeat(width.saturating_sub(used)))
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
    }

    /// Writes an executable stub file, for a test that needs a path that exists.
    fn stub_file(name: &str, body: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("omaread-stub-{name}"));
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
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

    #[test]
    fn a_book_and_a_subcommand_do_not_go_together() {
        let cli = |book: Option<&str>, chapter: Option<&str>| Cli {
            book: book.map(str::to_string),
            chapter: chapter.map(str::to_string),
            at: None,
            command: Some(Command::List {
                json: false,
                filter: None,
            }),
        };
        // Both halves used to be taken and one of them printed: the table, and
        // no reader at the chapter that was asked for.
        let err = check_invocation(&cli(Some("某本书"), Some("3")))
            .unwrap_err()
            .to_string();
        assert!(err.contains("do not go with a subcommand"), "{err}");
        let err = check_invocation(&cli(Some("某本书"), None))
            .unwrap_err()
            .to_string();
        assert!(err.contains("do not go with a subcommand"), "{err}");
        let err = check_invocation(&cli(None, Some("3")))
            .unwrap_err()
            .to_string();
        assert!(err.contains("do not go with a subcommand"), "{err}");
        // Neither on its own is exactly what was asked for.
        check_invocation(&cli(None, None)).unwrap();

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

    #[test]
    fn the_json_carries_everything_the_library_knows() {
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
        let entry = library::Entry {
            id: id.clone(),
            record,
        };

        let state = journal::State::default();
        let value = book_json(&entry, &state);
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

        // Where the reader stopped belongs in as well, when there is one.
        let journal_dir = std::env::temp_dir().join("omaread-test-book-json");
        std::fs::remove_dir_all(&journal_dir).ok();
        let mut journal = Journal::open(&journal_dir).unwrap();
        journal
            .record_position(
                &id,
                &doc::Locator {
                    href: "OEBPS/c1.xhtml".into(),
                    block: 4,
                    offset: 2,
                },
            )
            .unwrap();
        let state = Journal::replay(&journal_dir).unwrap();
        let value = book_json(&entry, &state);
        assert_eq!(value["position"]["href"], "OEBPS/c1.xhtml");
        assert_eq!(value["position"]["block"], 4);
        std::fs::remove_dir_all(&journal_dir).ok();
    }
}
