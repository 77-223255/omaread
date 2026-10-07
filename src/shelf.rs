//! Browsing the library.
//!
//! The state of the list view: which book the cursor is on, how the list is
//! ordered, what is filtered. Separate from `library`, which holds the data and
//! knows nothing about a screen.

use crate::i18n;
use crate::identity::BookId;
use crate::journal::State;
use crate::library::{self, Entry, Order};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Browse,
    /// Typing a filter.
    Filter,
    Help,
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
    /// Every book, unfiltered, in the current order.
    all: Vec<Entry>,
    /// What the list shows.
    shown: Vec<Entry>,
    cursor: usize,
    order: Order,
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
        let mut all = library::entries(state);
        // Title first: it is what the eye looks for, and it is the one field
        // almost every book fills in.
        let order = Order::Title;
        library::sort(&mut all, order);
        let shown = all.clone();
        Self {
            all,
            shown,
            cursor: 0,
            order,
            filter: String::new(),
            filter_input: String::new(),
            mode: Mode::Browse,
            status: None,
            pending: None,
            view_height: 1,
            scroll: 0,
            rows_area: (0, 0, 0, 0),
        }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.shown
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    pub fn order(&self) -> Order {
        self.order
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

    pub fn total(&self) -> usize {
        self.all.len()
    }

    /// Tells the shelf where its rows were drawn, so a click can name one.
    pub fn set_rows_area(&mut self, x: u16, y: u16, width: u16, height: u16) {
        self.rows_area = (x, y, width, height);
    }

    /// Opens the book a click landed on, if it landed on one.
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
        self.open_selected()
    }

    /// Moves the cursor with the wheel, one book a notch.
    pub fn handle_scroll(&mut self, up: bool) -> Action {
        if self.mode != Mode::Browse {
            return Action::None;
        }
        let last = self.shown.len().saturating_sub(1);
        self.cursor = if up {
            self.cursor.saturating_sub(1)
        } else {
            (self.cursor + 1).min(last)
        };
        self.status = None;
        self.follow_cursor();
        Action::None
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
            KeyCode::Esc => {
                if !self.filter.is_empty() {
                    self.filter.clear();
                    self.apply();
                    self.status = Some(i18n::t("filter cleared").into());
                } else {
                    self.status = None;
                }
            }
            // Cycles through the orders rather than needing four keys.
            KeyCode::Char('s') => {
                self.order = self.order.next();
                self.apply();
                self.status = Some(i18n::fill("sorted by {}", &[&i18n::t(self.order.label())]));
            }
            KeyCode::Enter | KeyCode::Char('l') => return self.open_selected(),
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
                    // The same sentence the command line answers with, in the
                    // words the reader was typed rather than quoted like code.
                    Some(i18n::fill("no book matches {}", &[&self.filter]))
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

    /// Re-sorts and re-filters, keeping the cursor on the same book where it
    /// still shows.
    fn apply(&mut self) {
        let selected = self.shown.get(self.cursor).map(|e| e.id.clone());
        library::sort(&mut self.all, self.order);
        self.shown = library::filter(&self.all, &self.filter);
        self.cursor = selected
            .and_then(|id| self.shown.iter().position(|e| e.id == id))
            .unwrap_or(0);
        self.follow_cursor();
    }

    fn open_selected(&mut self) -> Action {
        let Some(entry) = self.shown.get(self.cursor) else {
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
            ("Enter l", t("open the book")),
            ("/", t("filter by title, author, series or tag")),
            ("Esc", t("clear the filter")),
            ("s", t("cycle the order: title, author")),
            ("q", t("quit")),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::journal::BookRecord;
    use ratatui::crossterm::event::{KeyEventKind, KeyEventState, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn shelf_of(titles: &[&str]) -> Shelf {
        let all: Vec<Entry> = titles
            .iter()
            .enumerate()
            .map(|(i, title)| {
                let mut record = BookRecord::new(PathBuf::from(format!("/tmp/{i}.epub")));
                record.title = Some((*title).into());
                record.authors = vec![format!("Author {i}")];
                Entry {
                    id: BookId::from(format!("sha256:{i}")),
                    record,
                }
            })
            .collect();
        let mut shelf = Shelf {
            all: all.clone(),
            shown: all,
            cursor: 0,
            order: Order::Title,
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

    #[test]
    fn the_cursor_moves_stops_and_q_leaves() {
        let mut shelf = shelf_of(&["a", "b", "c"]);
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
    fn filtering_narrows_the_list_and_esc_restores_it() {
        let mut shelf = shelf_of(&["Anathem", "Cryptonomicon", "Seveneves"]);
        shelf.handle_key(key(KeyCode::Char('/')));
        for c in "crypto".chars() {
            shelf.handle_key(key(KeyCode::Char(c)));
        }
        shelf.handle_key(key(KeyCode::Enter));
        assert_eq!(shelf.entries().len(), 1);

        shelf.handle_key(key(KeyCode::Esc));
        assert_eq!(shelf.entries().len(), 3);

        // One sentence for one situation, and the word as it was typed: the
        // shelf and `omaread list --filter` answer the same way.
        shelf.handle_key(key(KeyCode::Char('/')));
        for c in "zzz".chars() {
            shelf.handle_key(key(KeyCode::Char(c)));
        }
        shelf.handle_key(key(KeyCode::Enter));
        assert!(shelf.entries().is_empty());
        assert_eq!(
            shelf.status(),
            Some(i18n::fill("no book matches {}", &[&"zzz"]).as_str())
        );
    }

    #[test]
    fn the_cursor_stays_on_its_book_when_the_order_changes() {
        let mut shelf = shelf_of(&["Zebra", "Apple", "Mango"]);
        // Sorted by title: Apple, Mango, Zebra. Put the cursor on Mango.
        shelf.handle_key(key(KeyCode::Char('j')));
        let before = shelf.entries()[shelf.cursor()].id.clone();
        shelf.handle_key(key(KeyCode::Char('s')));
        assert_eq!(shelf.entries()[shelf.cursor()].id, before);
    }

    #[test]
    fn opening_a_book_whose_file_is_gone_reports_it() {
        let mut shelf = shelf_of(&["Anathem"]);
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
    fn a_click_opens_the_row_it_landed_on() {
        let mut shelf = shelf_of(&["Anathem", "Cryptonomicon", "Seveneves"]);
        shelf.set_rows_area(0, 2, 40, 3);
        // The third row of the list, and the file behind it is gone, so the
        // click reports rather than opens — but the cursor still moved there.
        shelf.handle_click(3, 4);
        assert_eq!(shelf.cursor(), 2);
        // A click outside the rows leaves the cursor alone.
        shelf.handle_click(3, 5);
        assert_eq!(shelf.cursor(), 2);
        // And clicking when the rows are not there yet does nothing.
        let mut shelf = shelf_of(&["Anathem"]);
        shelf.handle_click(0, 0);
        assert_eq!(shelf.cursor(), 0);
    }

    #[test]
    fn a_click_follows_the_scroll() {
        let mut shelf = shelf_of(&["a", "b", "c", "d"]);
        shelf.prepare(2);
        shelf.handle_key(key(KeyCode::Char('G')));
        assert_eq!(shelf.scroll(), 2, "the last rows scrolled into view");
        // The top row of the view is the third book, not the first.
        shelf.set_rows_area(0, 0, 40, 2);
        shelf.handle_click(0, 0);
        assert_eq!(shelf.cursor(), 2);
    }

    #[test]
    fn a_click_in_the_filter_prompt_is_not_a_book() {
        let mut shelf = shelf_of(&["Anathem"]);
        shelf.set_rows_area(0, 0, 40, 1);
        shelf.handle_key(key(KeyCode::Char('/')));
        shelf.handle_click(0, 0);
        assert_eq!(shelf.mode, Mode::Filter, "the click did not open a book");
    }

    #[test]
    fn the_wheel_moves_one_row_a_notch() {
        let mut shelf = shelf_of(&["a", "b", "c", "d", "e"]);
        shelf.handle_scroll(false);
        assert_eq!(shelf.cursor(), 1);
        shelf.handle_scroll(false);
        assert_eq!(shelf.cursor(), 2);
        shelf.handle_scroll(true);
        assert_eq!(shelf.cursor(), 1);
        shelf.handle_scroll(true);
        assert_eq!(shelf.cursor(), 0, "must not go above the first");
    }

    #[test]
    fn the_view_follows_the_cursor() {
        let mut shelf = shelf_of(&["a", "b", "c", "d", "e", "f"]);
        shelf.prepare(3);
        shelf.handle_key(key(KeyCode::Char('G')));
        assert!(
            shelf.scroll() + 3 > shelf.cursor(),
            "cursor {} must be visible with scroll {}",
            shelf.cursor(),
            shelf.scroll()
        );
    }
}
