//! Browsing the library.
//!
//! The state of the list view: which author the cursor is on, whose books are
//! open, what is filtered. Separate from `library`, which holds the data and
//! knows nothing about a screen.
//!
//! Two levels, and one order for both: the authors alphabetically, and inside
//! an author the books by title. The order is fixed because a shelf nobody
//! configured has one shape, and a key that reshuffled it would only make the
//! shelf somewhere else than where it was left.

use crate::i18n;
use crate::identity::BookId;
use crate::journal::State;
use crate::library::{self, Author, Entry};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Browse,
    /// Typing a filter.
    Filter,
    Help,
}

/// Which list the shelf is on: every author, or the books of one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Authors,
    Books,
}

impl Level {
    /// The word for the list, so the status line can say which one it counts.
    pub fn label(self) -> &'static str {
        match self {
            Level::Authors => "authors",
            Level::Books => "books",
        }
    }
}

/// What the shelf asks the session to do next.
#[derive(Debug, Clone)]
pub enum Action {
    None,
    /// Open this book for reading.
    Open {
        id: BookId,
        path: PathBuf,
    },
    Quit,
}

pub struct Shelf {
    /// Every book, unfiltered. A shelf is a snapshot of the journal, so
    /// nothing adds a book while one is being browsed.
    all: Vec<Entry>,
    /// The books grouped by their first author, built by the first `apply` and
    /// kept: re-filtering is no reason to walk the library again to reach the
    /// groups it already holds. `None` until that first build.
    authors: Option<Vec<Author>>,
    /// The author whose books are open, as a position in `authors` — a
    /// position rather than a name because the group that names nobody must be
    /// enterable like any other, and it has no name to be held by. `None` means
    /// the shelf is on the author list.
    entered: Option<usize>,
    /// The author under the cursor when one was entered, so the way back can
    /// put the cursor on that author rather than on whatever row it holds now.
    back_to: Option<usize>,
    /// Which list the rows are: positions into `authors` on the first level and
    /// into `all` on the second, so a filter picks rows rather than copying
    /// books into a second shelf — the library is held once, and every
    /// keystroke moves indices, not records.
    shown: Vec<usize>,
    cursor: usize,
    filter: String,
    filter_input: String,
    pub mode: Mode,
    status: Option<String>,
    /// Pending first key of a sequence, such as `g` in `gg`.
    pending: Option<char>,
    /// Rows the list can show, set by the view before each draw.
    view_height: u16,
    scroll: usize,
    /// Where the rows were drawn last, as `(x, y, width, height)`. Set by the
    /// view so a click can be turned back into a row.
    rows_area: (u16, u16, u16, u16),
}

impl Shelf {
    pub fn new(state: &State) -> Self {
        // Authors first: a shelf is walked by writer, and the titles wait
        // behind the name they were written under.
        let mut shelf = Self {
            all: library::entries(state),
            authors: None,
            entered: None,
            back_to: None,
            shown: Vec::new(),
            cursor: 0,
            filter: String::new(),
            filter_input: String::new(),
            mode: Mode::Browse,
            status: None,
            pending: None,
            view_height: 1,
            scroll: 0,
            rows_area: (0, 0, 0, 0),
        };
        shelf.apply();
        shelf
    }

    /// The books grouped by author, once an `apply` has built them.
    fn groups(&self) -> &[Author] {
        self.authors.as_deref().unwrap_or(&[])
    }

    /// The author whose books are open, if any are.
    fn current_author(&self) -> Option<&Author> {
        self.entered.and_then(|index| self.groups().get(index))
    }

    /// Which list the shelf is on.
    pub fn level(&self) -> Level {
        match self.entered {
            Some(_) => Level::Books,
            None => Level::Authors,
        }
    }

    /// The rows of one list, or the empty list's rows.
    ///
    /// `shown` means authors at one level and books at the other, so a caller
    /// asking for the wrong one gets nothing rather than indices read as the
    /// wrong kind of row.
    fn level_rows(&self, level: Level) -> &[usize] {
        match self.level() == level {
            true => &self.shown,
            false => &[],
        }
    }

    /// The authors the list shows, each with the books it names: the rows of
    /// the first level.
    pub fn author_rows(&self) -> impl ExactSizeIterator<Item = (usize, &Author)> + '_ {
        self.level_rows(Level::Authors)
            .iter()
            .enumerate()
            .map(|(row, &index)| (row, &self.groups()[index]))
    }

    /// The books the list shows, each with the row it is drawn on.
    ///
    /// Rows read straight out of `all`, so a frame walks the shelf without
    /// copying anything and a re-filter that reshuffles rows moves no book.
    pub fn rows(&self) -> impl ExactSizeIterator<Item = (usize, &Entry)> + '_ {
        self.level_rows(Level::Books)
            .iter()
            .enumerate()
            .map(|(row, &index)| (row, &self.all[index]))
    }

    /// The book a row shows, when the shelf is on the books.
    pub fn entry(&self, row: usize) -> Option<&Entry> {
        if self.level() != Level::Books {
            return None;
        }
        self.shown.get(row).map(|&index| &self.all[index])
    }

    /// The name of the author whose books are open, for the status line.
    ///
    /// `None` on the author list, and on the group that names nobody — the
    /// view words that one itself, in the language the machine reads.
    pub fn author_name(&self) -> Option<&str> {
        self.current_author()?.name.as_deref()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// The filter prompt, while one is being typed.
    pub fn filter_input(&self) -> Option<&str> {
        if self.mode == Mode::Filter {
            Some(&self.filter_input)
        } else {
            None
        }
    }

    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    /// The rows without a filter: every author, or every book of the author
    /// whose books are open. What the summary counts the filtered rows against.
    pub fn total(&self) -> usize {
        match self.level() {
            Level::Authors => self.groups().len(),
            Level::Books => self.current_author().map_or(0, |author| author.books.len()),
        }
    }

    /// Tells the shelf where its rows were drawn, so a click can name one.
    pub fn set_rows_area(&mut self, x: u16, y: u16, width: u16, height: u16) {
        self.rows_area = (x, y, width, height);
    }

    /// Does what Enter would do on the row a click landed on, if it landed on
    /// one: opens the author at the top level, the book inside one.
    pub fn handle_click(&mut self, column: u16, row: u16) -> Action {
        if self.mode != Mode::Browse {
            return Action::None;
        }
        let (x, y, width, height) = self.rows_area;
        if width == 0
            || height == 0
            || column < x
            || column >= x + width
            || row < y
            || row >= y + height
        {
            return Action::None;
        }
        let index = self.scroll + (row - y) as usize;
        if index >= self.shown.len() {
            return Action::None;
        }
        self.cursor = index;
        self.follow_cursor();
        self.status = None;
        self.enter_or_open()
    }

    /// Moves the cursor with the wheel, one row a notch.
    pub fn handle_scroll(&mut self, up: bool) {
        if self.mode != Mode::Browse {
            return;
        }
        let last = self.shown.len().saturating_sub(1);
        self.cursor = if up {
            self.cursor.saturating_sub(1)
        } else {
            (self.cursor + 1).min(last)
        };
        self.status = None;
        self.follow_cursor();
    }

    /// Tells the shelf how many rows it has, before drawing.
    pub fn prepare(&mut self, height: u16) {
        self.view_height = height.max(1);
        self.follow_cursor();
    }

    fn follow_cursor(&mut self) {
        let height = self.view_height as usize;
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + height {
            self.scroll = self.cursor + 1 - height;
        }
        let max = self.shown.len().saturating_sub(height);
        self.scroll = self.scroll.min(max);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        if let Some(first) = self.pending.take()
            && first == 'g' && key.code == KeyCode::Char('g') {
                self.cursor = 0;
                self.follow_cursor();
                return Action::None;
            }
        match self.mode {
            Mode::Browse => self.handle_browse_key(key),
            Mode::Filter => {
                self.handle_filter_key(key);
                Action::None
            }
            Mode::Help => {
                self.mode = Mode::Browse;
                Action::None
            }
        }
    }

    fn handle_browse_key(&mut self, key: KeyEvent) -> Action {
        let last = self.shown.len().saturating_sub(1);
        let page = self.view_height.saturating_sub(2).max(1) as usize;

        match key.code {
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('j') | KeyCode::Down => {
                self.cursor = (self.cursor + 1).min(last);
                self.status = None;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                self.status = None;
            }
            KeyCode::Char(' ') | KeyCode::PageDown => self.cursor = (self.cursor + page).min(last),
            KeyCode::Backspace | KeyCode::PageUp => self.cursor = self.cursor.saturating_sub(page),
            KeyCode::Char('g') => self.pending = Some('g'),
            KeyCode::Char('G') => self.cursor = last,
            KeyCode::Char('/') => {
                self.filter_input = self.filter.clone();
                self.mode = Mode::Filter;
            }
            KeyCode::Esc => return self.go_back(),
            // Left is the same way out as `h`, and the same way out as Esc:
            // one key for going back, whichever shape the hand is in.
            KeyCode::Char('h') | KeyCode::Left => {
                if self.level() == Level::Books {
                    return self.go_back();
                }
            }
            KeyCode::Enter | KeyCode::Char('l') => return self.enter_or_open(),
            _ => {}
        }
        self.follow_cursor();
        Action::None
    }

    fn handle_filter_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Browse;
                self.filter_input.clear();
            }
            KeyCode::Enter => {
                self.filter = std::mem::take(&mut self.filter_input);
                self.mode = Mode::Browse;
                self.apply();
                self.status = if self.shown.is_empty() {
                    // One sentence per list: a word that finds no author and a
                    // word that finds no book are different complaints. The
                    // books keep the sentence `omaread list --filter` answers
                    // with, in the words as they were typed rather than
                    // quoted like code.
                    Some(match self.level() {
                        Level::Authors => i18n::fill("no author matches {}", &[&self.filter]),
                        Level::Books => i18n::fill("no book matches {}", &[&self.filter]),
                    })
                } else {
                    None
                };
            }
            KeyCode::Backspace => {
                self.filter_input.pop();
            }
            KeyCode::Char(c) => self.filter_input.push(c),
            _ => {}
        }
    }

    /// Re-filters the list the shelf is on, keeping the cursor on the same
    /// author or book where it still shows.
    fn apply(&mut self) {
        if self.authors.is_none() {
            // Built once: the shelf holds a snapshot the journal cannot change
            // under it, so the grouping only has to outlive re-filters — which
            // is what the `Some` says from here on.
            self.authors = Some(library::authors(&self.all));
        }
        match self.level() {
            Level::Authors => {
                let selected = self.shown.get(self.cursor).copied();
                self.shown = self.author_list();
                self.cursor = selected
                    .and_then(|author| self.shown.iter().position(|&index| index == author))
                    .unwrap_or(0);
            }
            Level::Books => {
                let selected = self.entry(self.cursor).map(|entry| entry.id.clone());
                self.shown = self.book_list();
                self.cursor = selected
                    .and_then(|id| {
                        self.shown
                            .iter()
                            .position(|&index| self.all[index].id == id)
                    })
                    .unwrap_or(0);
            }
        }
        self.follow_cursor();
    }

    /// The authors a filter leaves, in shelf order.
    fn author_list(&self) -> Vec<usize> {
        library::filter_authors(self.groups(), &self.filter)
    }

    /// The open author's books a filter leaves, in title order.
    fn book_list(&self) -> Vec<usize> {
        let Some(author) = self.current_author() else {
            return Vec::new();
        };
        if self.filter.trim().is_empty() {
            return author.books.clone();
        }
        // The matcher answers with positions in library order; asking it which
        // of the author's own books survive keeps the title order the grouping
        // already set instead of reshuffling the shelf into the library's.
        let matches: HashSet<usize> = library::filter(&self.all, &self.filter)
            .into_iter()
            .collect();
        author
            .books
            .iter()
            .copied()
            .filter(|index| matches.contains(index))
            .collect()
    }

    /// Enter — and `l`, and a click: an author opens its books, a book opens
    /// itself for reading.
    fn enter_or_open(&mut self) -> Action {
        let selected = match self.level() {
            Level::Books => return self.open_selected(),
            Level::Authors => self.shown.get(self.cursor).copied(),
        };
        let Some(author) = selected else {
            return Action::None;
        };
        self.back_to = Some(author);
        self.entered = Some(author);
        self.shown = self.book_list();
        // The books start at the first one: coming here to read, not to
        // resume a cursor inside a list that was never seen.
        self.cursor = 0;
        self.status = None;
        self.follow_cursor();
        Action::None
    }

    /// Esc, and `h` on the books: the filter first — undoing the word just
    /// typed is what the key has always done — then one level up, back to the
    /// authors with the cursor on the one that was left. On the authors there
    /// is nowhere further up to go.
    fn go_back(&mut self) -> Action {
        if !self.filter.is_empty() {
            self.filter.clear();
            self.apply();
            self.status = Some(i18n::t("filter cleared").into());
            return Action::None;
        }
        if self.level() == Level::Books {
            self.entered = None;
            self.shown = self.author_list();
            // The author remembered rather than the row it stood on: a word
            // typed inside the books may have shortened the list under it.
            self.cursor = self
                .back_to
                .and_then(|author| self.shown.iter().position(|&index| index == author))
                .unwrap_or(0);
            self.follow_cursor();
        }
        self.status = None;
        Action::None
    }

    fn open_selected(&mut self) -> Action {
        let Some(entry) = self.entry(self.cursor) else {
            return Action::None;
        };
        let Some(path) = entry.record.path().cloned() else {
            self.status = Some(i18n::t("no file recorded for this book").into());
            return Action::None;
        };
        if !path.exists() {
            // A file's name may hold anything, and this is shown to a
            // terminal: the path is cleaned on the way out.
            self.status = Some(i18n::fill(
                "file is gone: {}",
                &[&crate::journal::clean(&path.display().to_string())],
            ));
            return Action::None;
        }
        Action::Open {
            id: entry.id.clone(),
            path,
        }
    }

    /// The key bindings, for the help screen.
    pub fn bindings() -> Vec<(&'static str, &'static str)> {
        let t = crate::i18n::t;
        vec![
            ("j k ↓ ↑", t("down, up")),
            ("Space Backspace", t("page down, up")),
            ("gg G", t("first, last")),
            ("Enter l", t("enter the author, open the book")),
            ("/", t("filter by title, author, series or tag")),
            ("Esc", t("back to the authors, clear the filter")),
            ("q", t("quit")),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::journal::BookRecord;
    use crate::testkit::key;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Where a test book pretends to live: unique per shelf, so the tests
    /// running side by side do not share a file — least of all one that says
    /// it is gone while another test is holding it.
    fn book_path(shelf: usize, index: usize) -> PathBuf {
        std::env::temp_dir().join(format!(
            "omaread-shelf-{}-{shelf}-{index}.epub",
            std::process::id()
        ))
    }

    /// The file a shelf's book would open, read off the shelf itself rather
    /// than guessed: the shelf picked the path, and only it knows it.
    fn path_of(shelf: &Shelf, index: usize) -> PathBuf {
        shelf.all[index].record.path().unwrap().clone()
    }

    /// A shelf of `(title, author)` pairs; an empty author is a book that
    /// names nobody.
    fn shelf_of(books: &[(&str, &str)]) -> Shelf {
        static SHELVES: AtomicUsize = AtomicUsize::new(0);
        let shelf_no = SHELVES.fetch_add(1, Ordering::Relaxed);
        let all: Vec<Entry> = books
            .iter()
            .enumerate()
            .map(|(index, (title, author))| {
                let mut record = BookRecord::new(book_path(shelf_no, index));
                record.title = Some((*title).into());
                if !author.is_empty() {
                    record.authors = vec![(*author).into()];
                }
                Entry {
                    id: BookId::from(format!("sha256:{index}")),
                    record,
                }
            })
            .collect();
        let mut shelf = Shelf {
            all,
            authors: None,
            entered: None,
            back_to: None,
            shown: Vec::new(),
            cursor: 0,
            filter: String::new(),
            filter_input: String::new(),
            mode: Mode::Browse,
            status: None,
            pending: None,
            view_height: 10,
            scroll: 0,
            rows_area: (0, 0, 0, 0),
        };
        shelf.apply();
        shelf
    }

    /// The names on the author list, in the order they show.
    fn names(shelf: &Shelf) -> Vec<String> {
        shelf
            .author_rows()
            .map(|(_, author)| author.name.clone().unwrap_or_default())
            .collect()
    }

    /// The titles on the book list, in the order they show.
    fn titles(shelf: &Shelf) -> Vec<String> {
        shelf
            .rows()
            .map(|(_, entry)| entry.record.display_title())
            .collect()
    }

    /// Types a word into the filter and commits it.
    fn filter_by(shelf: &mut Shelf, word: &str) {
        shelf.handle_key(key(KeyCode::Char('/')));
        for c in word.chars() {
            shelf.handle_key(key(KeyCode::Char(c)));
        }
        shelf.handle_key(key(KeyCode::Enter));
    }

    #[test]
    fn the_shelf_opens_on_the_authors_and_shows_no_book_yet() {
        // Authors alphabetical by the same rule the books are titled by, and
        // every book accounted for behind the name it was written under.
        let shelf = shelf_of(&[
            ("Zebra", "Zena"),
            ("Apple", "Adam"),
            ("Mango", "Adam"),
            ("Orphan", ""),
        ]);
        assert_eq!(shelf.level(), Level::Authors, "the first screen is the authors");
        assert_eq!(names(&shelf), vec!["Adam", "Zena", ""]);
        let counts: Vec<usize> = shelf.author_rows().map(|(_, a)| a.books.len()).collect();
        assert_eq!(counts, vec![2, 1, 1]);
        assert_eq!(shelf.rows().len(), 0, "a book only shows behind its author");
        assert_eq!(shelf.total(), 3, "three groups to count against");
    }

    #[test]
    fn enter_opens_an_authors_books_and_the_way_back_restores_the_cursor() {
        let mut shelf = shelf_of(&[("Zebra", "Zena"), ("Apple", "Adam"), ("Mango", "Adam")]);
        shelf.handle_key(key(KeyCode::Char('j')));
        assert_eq!(shelf.cursor(), 1, "on Zena");

        shelf.handle_key(key(KeyCode::Enter));
        assert_eq!(shelf.level(), Level::Books);
        assert_eq!(shelf.cursor(), 0, "the books start at the first one");
        assert_eq!(titles(&shelf), vec!["Zebra"]);
        assert_eq!(shelf.author_name(), Some("Zena"));

        shelf.handle_key(key(KeyCode::Esc));
        assert_eq!(shelf.level(), Level::Authors);
        assert_eq!(shelf.cursor(), 1, "back where the author was left");

        // `l` and `h` are the same two keys in the other hand.
        shelf.handle_key(key(KeyCode::Char('l')));
        assert_eq!(shelf.level(), Level::Books);
        shelf.handle_key(key(KeyCode::Char('h')));
        assert_eq!(shelf.level(), Level::Authors);
        assert_eq!(shelf.cursor(), 1, "and the cursor is not moved by them");

        // Left leads out of the books too.
        shelf.handle_key(key(KeyCode::Left));
        assert_eq!(shelf.level(), Level::Authors, "already there");
        shelf.handle_key(key(KeyCode::Enter));
        shelf.handle_key(key(KeyCode::Left));
        assert_eq!(shelf.level(), Level::Authors, "the way back taken");
    }

    #[test]
    fn enter_on_a_book_asks_the_session_to_open_it() {
        let mut shelf = shelf_of(&[("Anathem", "Stephenson")]);
        let path = path_of(&shelf, 0);
        std::fs::write(&path, b"not really an epub").unwrap();
        shelf.handle_key(key(KeyCode::Enter));
        let action = shelf.handle_key(key(KeyCode::Enter));
        let _ = std::fs::remove_file(&path);
        match action {
            Action::Open { id, path: opened } => {
                assert_eq!(id, shelf.entry(0).unwrap().id);
                assert_eq!(opened, path);
            }
            other => panic!("expected the book to open, got {other:?}"),
        }
    }

    #[test]
    fn opening_a_book_whose_file_is_gone_reports_it() {
        let mut shelf = shelf_of(&[("Anathem", "Stephenson")]);
        assert!(
            !path_of(&shelf, 0).exists(),
            "nothing has written this file for the shelf"
        );
        shelf.handle_key(key(KeyCode::Enter));
        match shelf.handle_key(key(KeyCode::Enter)) {
            Action::None => {
                assert!(
                    shelf
                        .status()
                        .is_some_and(|s| s.contains(crate::i18n::t("file is gone")))
                )
            }
            other => panic!("expected no action, got {other:?}"),
        }
    }

    #[test]
    fn the_filter_narrows_whichever_list_is_showing() {
        let mut shelf = shelf_of(&[
            ("Hobbit", "Tolkien"),
            ("Silmarillion", "Tolkien"),
            ("Anathem", "Stephenson"),
        ]);
        filter_by(&mut shelf, "tolkien");
        assert_eq!(shelf.level(), Level::Authors);
        assert_eq!(names(&shelf), vec!["Tolkien"], "the filter picked the author");

        // Entering keeps the word, and on the books it is the books' own filter.
        shelf.handle_key(key(KeyCode::Enter));
        assert_eq!(shelf.level(), Level::Books);
        assert_eq!(
            titles(&shelf),
            vec!["Hobbit", "Silmarillion"],
            "both match his own name"
        );

        // Esc takes the word away first — on the books, where it was typed.
        shelf.handle_key(key(KeyCode::Esc));
        assert_eq!(shelf.level(), Level::Books, "still inside the author");
        assert_eq!(shelf.filter(), "", "and the filter is cleared");
        assert_eq!(titles(&shelf), vec!["Hobbit", "Silmarillion"]);

        // The next Esc is the way out, back to every author.
        shelf.handle_key(key(KeyCode::Esc));
        assert_eq!(shelf.level(), Level::Authors);
        assert_eq!(names(&shelf), vec!["Stephenson", "Tolkien"]);
    }

    #[test]
    fn a_word_that_finds_nothing_says_so_for_the_list_it_was_typed_on() {
        let mut shelf = shelf_of(&[("Hobbit", "Tolkien")]);
        filter_by(&mut shelf, "zzz");
        assert_eq!(shelf.rows().len(), 0);
        assert_eq!(shelf.author_rows().len(), 0);
        assert_eq!(
            shelf.status(),
            Some(i18n::fill("no author matches {}", &[&"zzz"]).as_str()),
            "at the authors, nothing matched an author"
        );

        // The same word inside one author is a complaint about books, in the
        // words the command line answers with.
        shelf.handle_key(key(KeyCode::Esc));
        shelf.handle_key(key(KeyCode::Enter));
        filter_by(&mut shelf, "zzz");
        assert_eq!(
            shelf.status(),
            Some(i18n::fill("no book matches {}", &[&"zzz"]).as_str())
        );
    }

    #[test]
    fn the_cursor_stays_on_the_same_author_when_the_filter_shrinks_the_list() {
        let mut shelf = shelf_of(&[("a", "Ann"), ("b", "Bob"), ("c", "Cid")]);
        shelf.handle_key(key(KeyCode::Char('G')));
        assert_eq!(shelf.cursor(), 2, "on Cid");
        filter_by(&mut shelf, "cid");
        assert_eq!(shelf.cursor(), 0, "the only one left, and it is still Cid");
        assert_eq!(names(&shelf), vec!["Cid"]);
    }

    #[test]
    fn the_cursor_moves_stops_and_q_leaves() {
        let mut shelf = shelf_of(&[("a", "Ann"), ("b", "Bob"), ("c", "Cid")]);
        shelf.handle_key(key(KeyCode::Char('k')));
        assert_eq!(shelf.cursor(), 0, "must not go above the first");
        for _ in 0..5 {
            shelf.handle_key(key(KeyCode::Char('j')));
        }
        assert_eq!(shelf.cursor(), 2, "must not go past the last");
        shelf.handle_key(key(KeyCode::Char('G')));
        assert_eq!(shelf.cursor(), 2);
        shelf.handle_key(key(KeyCode::Char('g')));
        shelf.handle_key(key(KeyCode::Char('g')));
        assert_eq!(shelf.cursor(), 0);
        // And q leaves.
        assert!(matches!(
            shelf.handle_key(key(KeyCode::Char('q'))),
            Action::Quit
        ));
    }

    #[test]
    fn a_click_is_enter_on_the_row_it_landed_on() {
        // One click, wherever it lands: the row under it is the row the cursor
        // takes, and the click does what Enter does there — so at the authors
        // it opens the author, and inside one it opens the book.
        let mut shelf = shelf_of(&[("a", "Ann"), ("b", "Bob"), ("c", "Cid")]);
        shelf.set_rows_area(0, 2, 40, 3);
        let action = shelf.handle_click(3, 4);
        assert_eq!(shelf.level(), Level::Books, "the third row was an author");
        assert_eq!(shelf.author_name(), Some("Cid"));
        assert_eq!(shelf.cursor(), 0, "inside, at the first of his books");
        assert!(
            matches!(action, Action::None),
            "the click entered the author; it did not open a book"
        );

        // A click outside the rows leaves the cursor alone.
        shelf.handle_click(3, 9);
        assert_eq!(shelf.cursor(), 0, "nothing under it, nothing moved");

        // Inside an author the same click on a book opens it: the row is
        // there, its file is not, which the shelf reports rather than opens.
        shelf.set_rows_area(0, 0, 40, 1);
        let action = shelf.handle_click(0, 0);
        assert!(matches!(action, Action::None));
        assert!(
            shelf
                .status()
                .is_some_and(|s| s.contains(crate::i18n::t("file is gone")))
        );

        // And clicking when the rows are not there yet does nothing.
        let mut shelf = shelf_of(&[("Anathem", "Stephenson")]);
        shelf.handle_click(0, 0);
        assert_eq!(shelf.level(), Level::Authors);

        // The view follows the cursor, so a click names the row the scrolled
        // view shows: the top row is the fourth author, not the first.
        let mut shelf = shelf_of(&[("a", "A"), ("b", "B"), ("c", "C"), ("d", "D")]);
        shelf.prepare(2);
        shelf.handle_key(key(KeyCode::Char('G')));
        assert_eq!(shelf.scroll(), 2, "the last rows scrolled into view");
        shelf.set_rows_area(0, 0, 40, 2);
        shelf.handle_click(0, 0);
        assert_eq!(shelf.level(), Level::Books);
        assert_eq!(shelf.author_name(), Some("C"), "the row that was showing");

        // In the filter prompt the rows are being filtered, not read: a click
        // there opens nothing at all.
        let mut shelf = shelf_of(&[("Anathem", "Stephenson")]);
        shelf.set_rows_area(0, 0, 40, 1);
        shelf.handle_key(key(KeyCode::Char('/')));
        shelf.handle_click(0, 0);
        assert_eq!(shelf.mode, Mode::Filter, "the click did not open a book");
    }

    #[test]
    fn the_wheel_moves_one_row_a_notch() {
        // Both directions, one row each — end-stopping is the cursor's own
        // rule, pinned with the keys.
        let mut shelf = shelf_of(&[("a", "A"), ("b", "B"), ("c", "C"), ("d", "D"), ("e", "E")]);
        shelf.handle_scroll(false);
        assert_eq!(shelf.cursor(), 1);
        shelf.handle_scroll(false);
        assert_eq!(shelf.cursor(), 2);
        shelf.handle_scroll(true);
        assert_eq!(shelf.cursor(), 1);
    }
}
