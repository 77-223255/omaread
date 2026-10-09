//! A reader open on a test book, and one frame of it drawn.
//!
//! Every test that drives the app spells out the same four steps — a journal
//! directory that cleans itself up, a book opened, an id hashed from the file,
//! a state with nothing in it — and then the same terminal a person would read
//! on. They are spelled out once here, so a test starts from an `App` and a
//! `Buffer` and spends its lines on what it is actually about.

use crate::app::App;
use crate::epub::Book;
use crate::identity::BookId;
use crate::journal::{Journal, State};
use crate::testkit::Scratch;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// A reader on `book_path` over a journal of its own: the directory is
/// scratch — unique to this run, removed when the test ends however it ends —
/// and the state is the one a first start has, so the book is read the way a
/// person who has never opened it reads it.
pub fn opened(
    name: &str,
    book_path: &std::path::Path,
    cell: crate::image::CellSize,
) -> (Scratch, App) {
    let dir = Scratch::new(name);
    let app = App::new(
        Book::open(book_path).unwrap(),
        BookId::of_file(book_path).unwrap(),
        Journal::open(&dir).unwrap(),
        &State::default(),
        cell,
    )
    .unwrap();
    (dir, app)
}

/// A reader on `book_path` over a journal of its own, in the cell size a
/// terminal that never said otherwise is read with — the cell almost every
/// test wants, spelled once.
pub fn opened_default(name: &str, book_path: &std::path::Path) -> (Scratch, App) {
    opened(name, book_path, crate::image::CellSize::default())
}

/// The page `app` paints in a terminal this size — what a reader of this book
/// on this window would see.
pub fn frame(app: &mut App, width: u16, height: u16) -> ratatui::buffer::Buffer {
    frame_with_placements(app, width, height).0
}

/// The page, and where each pixel escape was told to draw: a pixel protocol
/// paints outside the buffer, so a test that checks a picture against the
/// cells around it needs both halves of what one draw produced.
pub fn frame_with_placements(
    app: &mut App,
    width: u16,
    height: u16,
) -> (ratatui::buffer::Buffer, Vec<crate::ui::Placement>) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let mut placed = Vec::new();
    terminal
        .draw(|frame| placed = crate::ui::draw(frame, app))
        .unwrap();
    (terminal.backend().buffer().clone(), placed)
}
