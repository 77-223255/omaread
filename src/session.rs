//! The terminal, and every screen that runs on it.
//!
//! The reader has three ways in — the shelf, a book named on the command line,
//! a search hit — and each of them would take the terminal over for itself.
//! They did, and their orders had drifted: one path ran before the config and
//! the theme template existed. One session type, one builder and one event
//! loop sit here so that no screen can take the terminal over differently
//! from the screen next to it.

use crate::app::App;
use crate::commands::{Chapter, chapter_of, empty_library_hint};
use crate::epub::Book;
use crate::find;
use crate::identity::BookId;
use crate::journal::{self, Journal, State};
use crate::library;
use crate::paths;
use crate::report::shown;
use crate::shelf;
use crate::theme;
use crate::ui;
use anyhow::{Context, Result, bail};
use ratatui::backend::Backend;
use ratatui::buffer::{Buffer, Cell, CellDiffOption};
use ratatui::crossterm::event::{
    self, Event, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use std::io::Write;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Restores the usual reaction to a pipe nobody reads any more.
///
/// Rust ignores SIGPIPE, so a write into such a pipe returns an error and
/// `println!` turns that into a panic: `omaread list | head` ended in a
/// backtrace instead of simply stopping. The default action ends the process
/// quietly, which is how every other command behaves in a pipeline.
pub(crate) fn restore_sigpipe() {
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

/// Everything a reading session holds once it has a terminal.
///
/// The three entry paths each set this up for themselves, and their orders had
/// drifted: `find` never made the first start, so it could run before the
/// config and the theme template existed. One struct, one builder, one order.
struct Session {
    terminal: ratatui::DefaultTerminal,
    /// Lives as long as the session, so the mouse reporting goes off however
    /// the session ends — an error included.
    _mouse: MouseCapture,
    /// How pictures are drawn, asked once at startup.
    backend: crate::image::Backend,
    /// Pixel size of one cell, measured before the terminal is taken over.
    cell: crate::image::CellSize,
    /// The session's one theme watcher: it follows the reader from the shelf
    /// into a book and back, so the file is read once per session rather than
    /// once per screen or per book.
    theme: theme::Watcher,
    /// Where the journal lives, and what it held at the start of the session.
    /// The state is replayed again after every book, so a position made in one
    /// shows on the shelf.
    journal_dir: PathBuf,
    state: State,
    /// What the terminal is showing, carried from book to book so a book
    /// opened on a screen the shelf just drew starts from a wiped one rather
    /// than from a shadow that describes it. Every wipe in this module goes
    /// through `repaint_everything`, which empties it.
    presenter: Presenter,
}

impl Drop for Session {
    fn drop(&mut self) {
        // Here rather than at each caller: an error anywhere in a session must
        // still leave the terminal usable, and the message that reports it is
        // printed after this.
        ratatui::restore();
    }
}

/// Brings a reading session up, in the one order that works for every path.
///
/// `ensure_terminal` asks for a terminal when there is none; `first_start`
/// writes the config and the theme template, so `config` reads them instead of
/// falling back to defaults and the watcher built below has a theme to follow
/// from the very first frame — and its own word about what it hooked up lands
/// on the ordinary screen. The cell size is an `ioctl` and needs no raw mode.
/// Asking the terminal switches raw mode on and off itself, so it must run
/// before `ratatui::init` holds raw mode — crossterm's second enable is a
/// no-op while its disable would turn raw mode off under the init — and before
/// the alternate screen is up, so the terminal's answers cannot be mistaken
/// for user input later on. Both questions — how pictures are drawn, and which
/// colours the terminal draws with — share that one window, so startup pays
/// one wait rather than two. `ratatui::init` comes last.
///
/// `before_takeover` runs once the config is loaded and the journal replayed,
/// while the ordinary screen can still be written to: `find` gathers its hits
/// there, and a book opened from its path records itself in the journal.
/// `None` says there is nothing to draw, and the terminal is left alone.
fn start_session<T>(
    before_takeover: impl FnOnce(&Path, &mut State) -> Result<Option<T>>,
) -> Result<Option<(Session, T)>> {
    ensure_terminal()?;
    first_start()?;
    let config = paths::Config::load()?;
    let journal_dir = config.journal_dir()?;
    let mut state = Journal::replay(&journal_dir)?;
    let Some(prepared) = before_takeover(&journal_dir, &mut state)? else {
        return Ok(None);
    };
    let cell = cell_size();
    let (backend, colours) = ask_terminal(config.images.as_deref())?;
    let terminal = ratatui::init();
    let mouse = MouseCapture::begin();
    let theme = theme::Watcher::new(colours);
    Ok(Some((
        Session {
            terminal,
            _mouse: mouse,
            backend,
            cell,
            theme,
            journal_dir,
            state,
            presenter: Presenter::default(),
        },
        prepared,
    )))
}

/// Where the reader opens: the chapter a flag or a search hit named, and the
/// passage to look for in it. The shelf asks for neither.
#[derive(Default)]
struct Landing {
    chapter: Option<Chapter>,
    passage: Option<String>,
}

impl Session {
    /// Reads one book until the reader leaves it, saving where they stopped.
    ///
    /// Returns true when the reader quit for good; only the shelf, which would
    /// come back for another book, asks.
    fn read_book(&mut self, book: Book, id: BookId, landing: Landing) -> Result<bool> {
        let mut journal = Journal::open(&self.journal_dir)?;
        journal.assume_written(self.state.position(&id).cloned());
        let mut app = App::new(book, id, journal, &self.state, self.cell)?;
        app.set_image_backend(self.backend);
        // The colours the watcher holds now; the event loop keeps them current
        // from the same watcher while the book is open.
        app.set_theme(self.theme.theme());
        match landing.chapter {
            Some(Chapter::Number(number)) => app.go_to_chapter_number(number),
            Some(Chapter::Href(href)) => app.go_to_href(&href),
            None => {}
        }
        if let Some(passage) = landing.passage {
            app.search_for(passage);
        }

        repaint_everything(&mut self.terminal, &mut self.presenter)?;
        event_loop(
            &mut self.terminal,
            &mut app,
            &mut self.theme,
            &mut self.presenter,
        )?;
        app.save_position();
        Ok(app.should_quit_program)
    }
}

/// Opens the library and reads whichever book is picked, until the reader quits.
///
/// Shelf and reader are separate screens with one terminal between them. The
/// journal is replayed on every return, so a reading position made just now
/// shows on the shelf straight away.
pub(crate) fn browse() -> Result<()> {
    let Some((mut session, _)) = start_session(|_, _| Ok(Some(())))? else {
        return Ok(());
    };

    loop {
        session.state = Journal::replay(&session.journal_dir)?;
        let mut shelf = shelf::Shelf::new(&session.state);
        if shelf.total() == 0 {
            // The screen goes before the hint is printed on it.
            drop(session);
            empty_library_hint();
            return Ok(());
        }

        // The shelf runs until a book is picked or the reader quits.
        let picked = loop {
            session.theme.follow();
            let colours = session.theme.theme();
            session
                .terminal
                .draw(|frame| ui::draw_shelf(frame, &mut shelf, &colours))?;
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    match shelf.handle_key(key) {
                        shelf::Action::Open { id, path } => break Some((id, path)),
                        shelf::Action::Quit => break None,
                        shelf::Action::None => {}
                    }
                }
                Event::Mouse(mouse) => {
                    // A click that opened a row is the book to read.
                    match wish(&mouse) {
                        Wish::Press => {
                            if let shelf::Action::Open { id, path } =
                                shelf.handle_click(mouse.column, mouse.row)
                            {
                                break Some((id, path));
                            }
                        }
                        Wish::WheelUp => shelf.handle_scroll(true),
                        Wish::WheelDown => shelf.handle_scroll(false),
                        Wish::Nothing => {}
                    }
                }
                _ => {}
            }
        };

        let Some((id, path)) = picked else {
            return Ok(());
        };
        // A book that cannot be read keeps its place on the shelf rather than
        // ending the session.
        let Ok(book) = Book::open(&path) else {
            continue;
        };

        let quit = session.read_book(book, id, Landing::default())?;
        // Back to the shelf, with a clean screen: the reader may have left
        // pixels behind that no cell redraw would remove.
        repaint_everything(&mut session.terminal, &mut session.presenter)?;
        if quit {
            return Ok(());
        }
    }
}

/// Searches the library and opens whichever hit is picked.
///
/// The hits are gathered before the terminal is taken over, so progress from a
/// direct search is visible and a slow run can be interrupted — which is what
/// `start_session` hands the search as its `before_takeover` step. The config
/// and the theme template are made first, as in every other path.
pub(crate) fn find_and_open(query: &str) -> Result<()> {
    let ready = start_session(|_journal_dir, state| {
        let mut last = std::time::Instant::now();
        let results = find::find(query, state, 40, &mut |title| {
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
            // Nothing to open, so nothing to draw: the terminal stays as it was.
            return Ok(None);
        }
        Ok(Some(results))
    })?;

    // Pick a hit, then open the book there.
    let Some((mut session, results)) = ready else {
        return Ok(());
    };
    let picked = pick_hit(&mut session.terminal, &results, &mut session.theme)?;
    let Some(index) = picked else {
        return Ok(());
    };
    let hit = &results.hits[index];
    let path = find::file_of(hit, &session.state)?;
    let book =
        Book::open(&path).with_context(|| format!("cannot read {}", shown(path.display())))?;
    let landing = Landing {
        chapter: hit.chapter_href.clone().map(Chapter::Href),
        // The passage, so the reader lands on the sentence rather than the chapter.
        passage: Some(hit.passage.clone().unwrap_or_else(|| query.to_string())),
    };
    let id = hit.book.clone();
    session.read_book(book, id, landing)?;
    Ok(())
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
        theme.follow();
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

/// Opens a book, optionally at a chapter and a passage.
pub(crate) fn run_at(book: Book, path: PathBuf, chapter: Option<String>, at: Option<String>) -> Result<()> {
    // Before the terminal is taken over: a chapter that does not exist is an
    // error to read on an ordinary screen, not a status line in a reader that
    // has already swallowed the terminal.
    let target = chapter.map(|spec| chapter_of(&book, &spec)).transpose()?;

    // Recording the book runs before the takeover for the same reason: a
    // journal that cannot be written is news for an ordinary screen, and it
    // has to be in the journal before the reader shows the name it holds.
    let ready = start_session(|journal_dir, state| {
        let id = BookId::of_file(&path)?;

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
            let mut journal = Journal::open(journal_dir)?;
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

            // Replay once more when the book was just written. The reader shows the
            // name the library knows, and the library knows it only now that
            // `BookSeen` is in the journal: otherwise a book opened straight from
            // its path would be called "Untitled" at the foot of the screen while
            // the shelf, which reads the journal, calls it what its file is called.
            *state = Journal::replay(journal_dir)?;
        }
        Ok(Some(id))
    })?;
    let Some((mut session, id)) = ready else {
        return Ok(());
    };

    session.read_book(
        book,
        id,
        Landing {
            chapter: target,
            passage: at,
        },
    )?;
    Ok(())
}

/// Asks the terminal to report mouse presses for as long as this lives.
///
/// The cost is the terminal's own text selection: while mouse reporting is on,
/// a drag is reported here rather than selected. The reader buys a list that
/// can be clicked with it. A terminal that refuses costs only the mouse, so a
/// failed enable is not worth failing the session over.
///
/// Written by hand rather than taken from crossterm's `EnableMouseCapture`,
/// which also switches on any-motion tracking (modes 1002 and 1003): every
/// wiggle of the pointer would arrive as an event and cost a full frame that
/// draws nothing. Mode 1000 reports presses, releases and the wheel, 1006 says
/// to spell them SGR — that is all the loop answers, so that is all that is
/// asked for.
struct MouseCapture;

impl MouseCapture {
    fn begin() -> Self {
        // A failed write costs the mouse, not the session.
        let _ = write_modes(b"\x1b[?1000h\x1b[?1006h");
        MouseCapture
    }
}

impl Drop for MouseCapture {
    fn drop(&mut self) {
        // Here rather than at each caller: however the session ends — an error
        // included — the terminal must not be left reporting into a reader
        // that is no longer listening.
        let _ = write_modes(b"\x1b[?1006l\x1b[?1000l");
    }
}

/// Writes private-mode sequences to the terminal and flushes, so the change
/// is in effect before anything waits on what it reports.
fn write_modes(bytes: &[u8]) -> std::io::Result<()> {
    let mut out = std::io::stdout();
    out.write_all(bytes)?;
    out.flush()
}

/// What a mouse event asks of a screen: the press, one notch of the wheel
/// either way, or nothing at all.
///
/// Settled before the dispatch so the event loop can ask the same question
/// before drawing: a frame owed to a screen is a frame owed for what the
/// screen was offered.
enum Wish {
    Press,
    WheelUp,
    WheelDown,
    Nothing,
}

/// Which of the three actions a mouse event carries, if any.
///
/// Motion, the other buttons and a release are nothing on any screen, and
/// that has to mean the same thing to the loop as to the dispatch.
fn wish(mouse: &MouseEvent) -> Wish {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => Wish::Press,
        MouseEventKind::ScrollUp => Wish::WheelUp,
        MouseEventKind::ScrollDown => Wish::WheelDown,
        _ => Wish::Nothing,
    }
}

/// How long a frame-earning key waits for the rest of its burst.
///
/// Each look for the next key is short so a lone key on a slow link is on
/// screen within a couple of milliseconds, and the whole budget is short so a
/// held key cannot postpone the frame that shows what it did. Together they
/// turn a burst into one frame without turning one press into a wait.
const KEY_POLL: Duration = Duration::from_millis(2);
const KEY_BUDGET: Duration = Duration::from_millis(10);

/// How long the wait runs when the round just past earned no frame.
///
/// An ignored event owes no frame, but the screen may still owe one — only a
/// resize among them changes what should be there without changing the app —
/// so the wait ends after a beat and draws it. Long enough that a flood of
/// pointer events costs no frames while it lasts.
const SKIP_TIMEOUT: Duration = Duration::from_millis(300);

/// How long the wait runs while a picture is still being decoded elsewhere.
///
/// A decode owes the loop no keypress, so a wait with one outstanding must
/// not block: it runs out on a clock instead, and the timeout draws the frame
/// that puts the finished picture on the screen. Short, so a page of pictures
/// appears as they land rather than all at once at the next key; long enough
/// that a chapter of them does not turn the loop into a spin.
const PICTURE_POLL: Duration = Duration::from_millis(40);

/// How long the view must stand still before pictures are decoded for it.
///
/// A decode costs milliseconds and a held scroll key moves every 30–50 ms:
/// anything shorter would never outlast a hold, and every decode started
/// mid-flight lands on a page the reader has already scrolled past. Long
/// enough to skip whole bursts, short enough that a reader who stops barely
/// notices the wait.
const SETTLE: Duration = Duration::from_millis(180);

/// The view's motion as the draw sees it: when it last moved, and whether it
/// has stood still long enough for pictures to be decoded for it.
///
/// Pure arithmetic over the instants it is handed, so the decision is tested
/// against a clock the test makes rather than one it waits on.
#[derive(Default)]
struct Settle {
    last_motion: Option<Instant>,
}

impl Settle {
    /// The view has just moved, so the settle window starts now.
    fn moved(&mut self, now: Instant) {
        self.last_motion = Some(now);
    }

    /// Whether decoding may go ahead: the view has never moved under this
    /// session's eye, or it has stood still for a whole window.
    fn allows(&self, now: Instant) -> bool {
        self.last_motion
            .is_none_or(|at| now.saturating_duration_since(at) >= SETTLE)
    }
}

/// A frame on the terminal, sent as a scroll when the page only moved.
///
/// Holding a scroll key moves the page a row at a time, and one moved row
/// changes every cell: ratatui writes the whole screen's diff — about 4 KB at
/// 50 columns of Chinese text — for each notch, and a slow link has to carry
/// all of it. A terminal can move its own rows, so the fast path here sends
/// one short escape for the scroll and the cells the scroll exposed: about a
/// row's worth, plus whatever the status line changed.
///
/// It applies only when the frame is exactly what the screen already holds,
/// moved whole rows: every row the scroll does not expose is compared cell
/// for cell — symbol and style — with the row it came from, and only exact
/// equality is enough, because those rows are moved and never written. Any
/// other change (a focus that dimmed, a cursor that moved, a status that
/// changed, a frame after a wipe or a resize) fails the test and falls back
/// to `Terminal::flush`.
///
/// The fallback is always correct because it is the call a frame has always
/// made: ratatui's diff against the buffer it wrote last, which it keeps for
/// itself whatever this presenter remembers. The shadow is read only for the
/// scroll, where exactness is what makes it safe — and it is exact because
/// both paths rebuild it from the buffer they just presented, while a wipe
/// or a resize empties it so that nothing can be scrolled into place from a
/// screen the terminal no longer shows.
#[derive(Default)]
struct Presenter {
    /// What the terminal is showing, cell for cell: the frame last presented,
    /// copied out of ratatui's own buffer. Held as a buffer so a frame can be
    /// diffed against it the way ratatui diffs its two buffers.
    shadow: Buffer,
    /// The frame as it will be written: a copy of it, so that the cells
    /// handed to the backend do not borrow the terminal — the backend is
    /// reached through the terminal while they are being written.
    outgoing: Buffer,
    /// Cells the last frame handed the backend: what a notch of scrolling
    /// actually cost, counted for the tests that have no other way to see it.
    sent: usize,
    /// Whether the last frame moved the screen rather than redrawing it.
    scrolled: bool,
}

/// Which way a frame moved the screen, and by how many rows.
#[derive(Clone, Copy)]
enum Shift {
    /// The frame is the screen moved `u16` rows up: the rows at the bottom of
    /// the region are new.
    Up(u16),
    /// The frame is the screen moved `u16` rows down: the rows at the top of
    /// the region are new.
    Down(u16),
}

impl Shift {
    const fn rows(self) -> u16 {
        match self {
            Shift::Up(rows) | Shift::Down(rows) => rows,
        }
    }
}

impl Presenter {
    /// Puts one rendered frame on the terminal, and returns its placements.
    ///
    /// `render` is handed a frame of the terminal's own buffer, exactly as
    /// `Terminal::draw` would hand it one; `scrollable` says the screen holds
    /// nothing but cells, because a picture painted outside the buffer would
    /// be moved by a scroll while the record of where it sits says it never
    /// moved.
    fn present<B: Backend, F>(
        &mut self,
        terminal: &mut ratatui::Terminal<B>,
        scrollable: bool,
        render: F,
    ) -> Result<Vec<ui::Placement>, B::Error>
    where
        F: FnOnce(&mut ratatui::Frame) -> Vec<ui::Placement>,
    {
        // The terminal is asked its size rather than trusting the last one: a
        // resize between frames has to be drawn into from scratch, and the
        // scroll below only describes a screen the shadow matches. The buffer's
        // own area is checked too, so a resize made elsewhere (a wipe) can
        // never be scrolled against a shadow from before it.
        let size = terminal.size()?;
        let area = Rect::new(0, 0, size.width, size.height);
        let buffer_area = *terminal.current_buffer_mut().area();
        if !scrollable
            || self.shadow.content.is_empty()
            || self.shadow.area != area
            || self.shadow.area != buffer_area
        {
            return self.redraw(terminal, render);
        }

        // The rows the scroll may move: the page and everything above the
        // status row, so a status that changes on its own never has to fit a
        // shift, and the whole screen when the frame has no status row.
        let (text, _status) = ui::split_frame(area);
        let region = text.top()..text.bottom();

        let placements = {
            let mut frame = terminal.get_frame();
            render(&mut frame)
        };

        // What the frame asks for, decided while the buffer can still be read;
        // the backend is reached only once no borrow of it is left.
        let shift = {
            let fresh = terminal.current_buffer_mut();
            let shift = self.shift(fresh, &region);
            if shift.is_some() {
                self.copy_frame(fresh);
            }
            shift
        }
        .filter(|&shift| self.worth_scrolling(shift, &region));

        let Some(shift) = shift else {
            // Nothing moved in rows — or the move would write as many cells as
            // the redraw it replaces — so the frame is written the way every
            // frame used to be written: ratatui's diff against the buffer it
            // presented last, which is what the terminal is showing.
            terminal.flush()?;
            self.sent = 0;
            self.scrolled = false;
            return self.finish(terminal, placements);
        };

        // The escape goes out first: what follows is what the screen should
        // show once the rows have moved.
        let rows = shift.rows();
        match shift {
            Shift::Up(_) => terminal.backend_mut().scroll_region_up(region.clone(), rows)?,
            Shift::Down(_) => terminal.backend_mut().scroll_region_down(region.clone(), rows)?,
        }

        // What is written: every cell of the rows the scroll exposed — a
        // terminal fills them itself, in a colour this program never chose —
        // and the cells outside the region that differ, which is the status
        // row. The rows the scroll moved into place are not written at all;
        // that is the whole saving.
        let mut sent = 0usize;
        let exposed = exposed_rows(shift, &region);
        let outside = self
            .shadow
            .diff_iter(&self.outgoing)
            .filter(|(_, y, _)| !region.contains(y));
        terminal.backend_mut().draw(
            exposed_cells(&self.outgoing, exposed)
                .chain(outside)
                .inspect(|_| sent += 1),
        )?;
        self.sent = sent;
        self.scrolled = true;
        self.finish(terminal, placements)
    }

    /// Presents the frame the ordinary way: rendered into the terminal and
    /// written by ratatui's own diff.
    ///
    /// `draw` swaps buffers and flushes on its way out, so the frame it showed
    /// is the *other* buffer by the time it returns — `current_buffer_mut` is
    /// already the empty one the next frame renders into — and the completed
    /// frame is what has to be remembered as what the terminal shows.
    fn redraw<B: Backend, F>(
        &mut self,
        terminal: &mut ratatui::Terminal<B>,
        render: F,
    ) -> Result<Vec<ui::Placement>, B::Error>
    where
        F: FnOnce(&mut ratatui::Frame) -> Vec<ui::Placement>,
    {
        let mut placements = Vec::new();
        let shown = terminal.draw(|frame| placements = render(frame))?;
        self.shadow.area = *shown.buffer.area();
        self.shadow.content.clone_from(&shown.buffer.content);
        self.sent = 0;
        self.scrolled = false;
        Ok(placements)
    }

    /// Forgets what is on screen, so the next frame is written in full.
    ///
    /// The screen a shadow describes can be wiped out from under it — a view
    /// that moved, a theme that changed, a resize — and a scroll applied to a
    /// screen that no longer holds those cells would put them back.
    fn invalidate(&mut self) {
        self.shadow.area = Rect::ZERO;
        self.shadow.content.clear();
    }

    /// Remembers a frame as what the terminal now shows, and hands the buffer
    /// over for the next one.
    ///
    /// This is what both paths end with, and it is what keeps the next frame's
    /// comparison honest: the shadow is always the buffer that was handed to
    /// the terminal, never a guess about it. The swap keeps ratatui's own
    /// previous buffer in step too, so `Terminal::flush` still writes the right
    /// cells on the frames that fall back to it.
    fn finish<B: Backend>(
        &mut self,
        terminal: &mut ratatui::Terminal<B>,
        placements: Vec<ui::Placement>,
    ) -> Result<Vec<ui::Placement>, B::Error> {
        let shown = terminal.current_buffer_mut();
        self.shadow.area = *shown.area();
        self.shadow.content.clone_from(&shown.content);
        terminal.swap_buffers();
        terminal.backend_mut().flush()?;
        Ok(placements)
    }

    /// The uniform vertical shift that turns the screen into `fresh`, if the
    /// frame is the screen moved whole rows inside `region`.
    ///
    /// Several shifts can fit when a page repeats lines — a paragraph that
    /// wraps the same way every time puts identical rows a page apart — so the
    /// one chosen is the smallest that fits: it is backed by the most rows
    /// (every row but the span it moves, compared exactly, symbol and style),
    /// where a larger shift may fit on the strength of a single row that
    /// happened to line up, and it leaves the fewest rows to write. The
    /// smallest is also the cheapest: a larger span only ever moves more rows
    /// into the part the terminal fills in itself.
    ///
    /// Exactness is what makes any of them safe, since the rows a shift moves
    /// are never written — the terminal moves them. Rows outside the region are
    /// not compared — the status row may change on its own — and are left for
    /// the diff to write.
    fn shift(&self, fresh: &Buffer, region: &Range<u16>) -> Option<Shift> {
        let width = self.shadow.area.width as usize;
        let top = region.start as usize;
        let rows = (region.end - region.start) as usize;
        let on_screen = self.shadow.content.as_slice();
        let wanted = fresh.content.as_slice();

        for span in 1..rows {
            let overlap = rows - span;
            if (0..overlap).all(|k| row(on_screen, width, top + k + span) == row(wanted, width, top + k))
            {
                return Some(Shift::Up(span as u16));
            }
            if (0..overlap).all(|k| row(wanted, width, top + k + span) == row(on_screen, width, top + k))
            {
                return Some(Shift::Down(span as u16));
            }
        }
        None
    }

    /// Copies the frame into `outgoing`.
    ///
    /// The cells that go to the backend have to live somewhere the terminal is
    /// not: `backend_mut` is reached through the terminal while they are being
    /// written, so a frame borrowed from it could not be held at the same time.
    fn copy_frame(&mut self, fresh: &Buffer) {
        self.outgoing.area = *fresh.area();
        self.outgoing.content.clone_from(&fresh.content);
    }

    /// Whether moving the screen by `shift` writes fewer cells than redrawing
    /// the frame would.
    ///
    /// The scroll pays for every cell of the rows it exposes — a terminal
    /// fills those rows itself, in a colour this program never chose — and
    /// saves the cells it moves into place. A shift that fits only because one
    /// repeated row lined up exposes a screenful of rows to fill and saves
    /// nothing, so it is not worth the escape it would send; the same is true
    /// of a frame whose only change is outside the region, where the status
    /// row lives.
    fn worth_scrolling(&self, shift: Shift, region: &Range<u16>) -> bool {
        let exposed = exposed_cells(&self.outgoing, exposed_rows(shift, region)).count();
        let changed = self
            .shadow
            .diff_iter(&self.outgoing)
            .fold(0usize, |count, (_, y, _)| count + usize::from(region.contains(&y)));
        exposed < changed
    }
}

/// One row of a flat buffer, for the comparisons in `Presenter::shift`.
fn row(cells: &[Cell], width: usize, index: usize) -> &[Cell] {
    &cells[index * width..(index + 1) * width]
}

/// The rows a scroll of `region` by `shift` leaves empty for the frame to fill.
fn exposed_rows(shift: Shift, region: &Range<u16>) -> Range<u16> {
    let rows = shift.rows();
    match shift {
        Shift::Up(_) => region.end - rows..region.end,
        Shift::Down(_) => region.start..region.start + rows,
    }
}

/// The cells of the rows a scroll exposed, in the order they are written:
/// every one of them, because a terminal fills those rows itself in a colour
/// this program never chose — except the column after a wide character, which
/// the terminal fills in when it writes the character, and a cell that
/// something else is drawing.
fn exposed_cells(outgoing: &Buffer, rows: Range<u16>) -> impl Iterator<Item = (u16, u16, &Cell)> {
    let area = outgoing.area;
    rows.flat_map(move |y| {
        (area.x..area.right()).filter_map(move |x| {
            let cell = &outgoing[(x, y)];
            let after_wide = x > area.x && crate::measure::cells(outgoing[(x - 1, y)].symbol()) > 1;
            if after_wide || cell.diff_option == CellDiffOption::Skip {
                None
            } else {
                Some((x, y, cell))
            }
        })
    })
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    theme: &mut theme::Watcher,
    presenter: &mut Presenter,
) -> Result<()> {
    let mut last_token = None;
    // When the view last moved, and therefore how long before new decodes
    // may go out: a picture decoded for a page the reader has already
    // scrolled past is a picture thrown away.
    let mut settle = Settle::default();
    // Whether the previous frame put pixels on screen. Asking the app instead
    // would come too late: on a chapter change the old chapter's pictures are
    // already gone and the new one's are not rendered yet.
    let mut pixels_on_screen = false;
    // What the terminal is holding, so a frame that did not move a picture
    // writes nothing: kitty pictures stay where they are until deleted, and
    // their payloads travelled once, when they were first rendered.
    let mut screen = Screen::default();
    // The first frame goes out before any event is read, so a book is on
    // screen without waiting for a keypress. From then on a frame is drawn
    // only when an event earned one: the frame used to be drawn before the
    // event was read, so every event the loop ignores — pointer motion above
    // all — paid for a full redraw that changed nothing.
    let mut dirty = true;
    // Whether the round just past earned no frame. The wait for the next
    // event then runs out after a beat instead of blocking, which is what
    // puts the skipped frame on the screen when the events dry up.
    let mut skipped = false;

    while !app.should_leave_book {
        if dirty {
            // Pixel pictures are not part of the cell buffer, so a diffed redraw
            // leaves them wherever no character happens to overwrite them. Once the
            // view has moved, the screen has to be wiped and painted afresh.
            let token = app.frame_token();
            let moved = last_token.is_some_and(|last| last != token);
            // Decoding waits for the view to settle. The frame that follows
            // the settle is told to ask, and the busy clock below keeps the
            // loop running until it does. A chapter, a room or a backend
            // that changed hands answers for itself: those raise the
            // generation inside the draw below and allow decoding at once.
            let now = Instant::now();
            if moved {
                settle.moved(now);
            }
            app.set_decode_allowed(settle.allows(now));
            // Either direction matters: pictures that are on screen have to go, and
            // pictures that are about to appear need a clean surface.
            let wiping = moved && (pixels_on_screen || app.has_pixel_images());
            last_token = Some(token);

            {
                // The wipe above and the painting below are one frame to the reader,
                // so the terminal is told to hold the display until both are done.
                let _frame = HeldDisplay::begin();
                // A wipe hands the surface back, so the pictures have to be painted
                // again even if the frame asks for the very same ones.
                let mut forced = false;
                if wiping {
                    repaint_everything(terminal, presenter)?;
                    forced = true;
                }
                // A theme switch replaces the colours under us. Checking after each
                // key is enough and costs one stat call — and following it asks the
                // terminal its colours again, since the switch re-coloured it too.
                if theme.follow() {
                    app.set_theme(theme.theme());
                    repaint_everything(terminal, presenter)?;
                    forced = true;
                }

                // A picture is painted outside the cell buffer, so scrolling
                // the cells under one would move it while the record of where
                // it sits says nothing moved. Such a screen is redrawn, never
                // scrolled — and scrolling it would save nothing, since a view
                // that moved with pictures on it is wiped first anyway.
                let scrollable = !pixels_on_screen && !app.has_pixel_images();
                let placements =
                    presenter.present(terminal, scrollable, |frame| ui::draw(frame, app))?;
                place_images(app, &placements, &mut screen, forced)?;
                pixels_on_screen = !placements.is_empty();
            }
            skipped = false;
        }

        // After a frame nothing is owed, so the next event is waited for
        // without a limit; after a skipped round the wait times out, so even
        // an event the loop ignores cannot leave the screen stale for ever.
        // A picture still being decoded owes the screen a frame the same way,
        // and on the shorter clock: its answer has no key to wake the loop
        // with, so the timeout is what puts it on the screen.
        let decoding = app.pictures_busy();
        let wait = if decoding { PICTURE_POLL } else { SKIP_TIMEOUT };
        if (skipped || decoding) && !event::poll(wait)? {
            dirty = true;
            continue;
        }
        let first = event::read()?;

        // A held-down key delivers keys faster than a chapter full of pictures
        // can be painted, and a slow link delivers one key and then nothing.
        // So what is already waiting is taken for a small budget — a couple of
        // milliseconds at a time, ten in all — and one frame shows the lot.
        let is_key = matches!(&first, Event::Key(key) if key.kind == KeyEventKind::Press);
        let mut changes = handle_event(app, &first);
        if is_key {
            let deadline = std::time::Instant::now() + KEY_BUDGET;
            while !app.should_leave_book
                && std::time::Instant::now() < deadline
                && event::poll(KEY_POLL)?
            {
                if handle_event(app, &event::read()?) {
                    changes = true;
                }
            }
        }
        dirty = changes;
        skipped = !changes;

        // A vision-mode yank leaves the text here, and the terminal's
        // clipboard is the one thing the reader cannot write by itself.
        if let Some(text) = app.take_clipboard() {
            copy_to_clipboard(&text);
        }
    }
    Ok(())
}

/// Applies one event to the book, saying whether the frame owes the reader an
/// update.
///
/// Keys always do: what a key does is visible, and the loop must not swallow
/// the frame that shows it. The mouse does only when it is an event
/// `wish` names — a press, a notch of the wheel — because motion, a
/// release and focus leave the app as it was, and a frame that changes nothing
/// still costs a full redraw: pointer motion used to spend 200 us of text, or
/// 830 us of picture, per wiggle. A resize changes the screen rather than the
/// app, so its frame comes from the wait's timeout.
fn handle_event(app: &mut App, event: &Event) -> bool {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            app.handle_key(*key);
            true
        }
        Event::Mouse(mouse) => match wish(mouse) {
            Wish::Press => {
                app.handle_click(mouse.column, mouse.row);
                true
            }
            Wish::WheelUp => {
                app.handle_scroll(true);
                true
            }
            Wish::WheelDown => {
                app.handle_scroll(false);
                true
            }
            Wish::Nothing => false,
        },
        _ => false,
    }
}

/// Puts text on the terminal's clipboard with OSC 52.
///
/// The sequence is the terminal's own clipboard instruction: it works over
/// ssh as readily as locally, and a terminal that does not know it ignores
/// it. The text travels base64-encoded, and the whole escape is written at
/// once: a partial sequence would leave a corrupted clipboard behind.
fn copy_to_clipboard(text: &str) {
    use base64::Engine;
    use std::io::Write;
    let payload = base64::engine::general_purpose::STANDARD.encode(text);
    let mut out = std::io::stdout();
    // Failing to copy costs the clipboard, not the session.
    let _ = write!(out, "\x1b]52;c;{payload}\x1b\\");
    let _ = out.flush();
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
///
/// The presenter is emptied with the screen: what it remembers is the frame
/// the wipe just removed, and a scroll applied to it would put those cells
/// back on a screen that no longer holds them.
fn repaint_everything(
    terminal: &mut ratatui::DefaultTerminal,
    presenter: &mut Presenter,
) -> Result<()> {
    use ratatui::backend::Backend;
    let size = terminal.size().context("cannot read the terminal size")?;
    terminal.backend_mut().clear().context("cannot clear")?;
    terminal
        .resize(ratatui::layout::Rect::new(0, 0, size.width, size.height))
        .context("cannot reset the buffers")?;
    presenter.invalidate();
    Ok(())
}

/// Decides how pictures are drawn, and learns which colours the terminal
/// draws with — both questions in the one raw-mode window.
///
/// Asking needs raw mode, because the answers arrive as escape sequences on
/// stdin. Raw mode is switched off again straight away, so a terminal that
/// stays silent leaves nothing behind. This is the session's one window of
/// the kind, so the colour question is asked in it rather than opening a
/// second one at startup.
fn ask_terminal(
    images: Option<&str>,
) -> Result<(crate::image::Backend, theme::TerminalColors)> {
    use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode};

    // A setting wins over asking about pictures; a misspelling must not be
    // read as "no pictures", so it falls through to the question.
    let mut fixed = None;
    if let Some(setting) = images {
        match crate::image::Backend::parse(setting) {
            Some(backend) => fixed = Some(backend),
            None => eprintln!(
                "omaread: unknown images setting {setting:?}, asking the terminal instead"
            ),
        }
    }
    let pixels = crate::image::detect::pixels_possible();
    let ask = format!("{}{}", crate::image::detect::ASK, theme::ASK);

    enable_raw_mode().context("cannot enter raw mode to query the terminal")?;
    let reply = crate::tty::gather(&ask, theme::TIMEOUT, theme::QUIET);
    disable_raw_mode().ok();

    let backend = match fixed {
        Some(backend) => backend,
        None if pixels => crate::image::detect::classify(&reply),
        None => crate::image::Backend::Quad,
    };
    Ok((backend, theme::TerminalColors::parse(&reply)))
}

/// Pixel size of one cell, as reported by the terminal. Falls back to a common
/// default when the terminal does not say.
fn cell_size() -> crate::image::CellSize {
    use ratatui::crossterm::terminal::window_size;
    match window_size() {
        Ok(size) if size.width > 0 && size.height > 0 && size.columns > 0 && size.rows > 0 => {
            crate::image::CellSize {
                width: size.width / size.columns,
                height: size.height / size.rows,
            }
        }
        _ => crate::image::CellSize::default(),
    }
}

/// What the terminal is holding between frames: the placements last written,
/// so a frame that moved nothing writes nothing, and the kitty ids it holds,
/// so a picture whose rendered form is already out of the cache can still be
/// deleted by the id the terminal knows it as.
#[derive(Default)]
struct Screen {
    placements: Vec<ui::Placement>,
}

/// Writes the pictures of a frame onto the screen.
///
/// Pixel protocols paint outside the text buffer, so ratatui knows nothing
/// about them. Each picture is placed at the cursor position its reserved
/// lines start at — whole when the view shows all of it, and cropped to what
/// is visible when it does not.
///
/// The terminal keeps kitty pictures until they are deleted, so a frame whose
/// pictures did not move writes nothing at all, one that did moves them by
/// id — the payload travelled when the picture was first rendered and is not
/// printed again — and one that no longer shows a picture deletes that id
/// instead of every picture on screen. `forced` says the surface was just
/// wiped, when the same pictures still have to be painted again.
fn place_images(
    app: &App,
    placements: &[ui::Placement],
    screen: &mut Screen,
    forced: bool,
) -> Result<()> {
    let now: Vec<ui::Placement> = placements.to_vec();
    if !forced && now == screen.placements {
        return Ok(());
    }

    let kitty = app.image_backend() == crate::image::Backend::Kitty;
    if !kitty && placements.is_empty() {
        // Sixel is part of the screen contents: with nothing to draw there is
        // nothing to write and nothing to remember.
        *screen = Screen::default();
        return Ok(());
    }

    use ratatui::crossterm::cursor::{MoveTo, RestorePosition, SavePosition};
    use ratatui::crossterm::queue;
    use ratatui::crossterm::style::Print;
    let mut out = std::io::stdout();

    // Kitty pictures are not part of the cell buffer: the frame's text does
    // not erase the ones the last frame left behind, so every changed frame
    // takes all of them away and paints what it wants. That is one short
    // escape, and the memo above keeps the frames that changed nothing from
    // paying even that.
    if kitty {
        queue!(out, Print(crate::image::kitty::delete_all()))?;
    }

    for placement in placements {
        // The view only places a picture it has rendered, so the escape is
        // always there; looking it up here rather than carrying it in the
        // placement keeps every frame from copying the bytes.
        let Some(rendered) = app.image_at(placement.block) else {
            continue;
        };
        let (from, visible) = placement.crop;

        if kitty {
            let whole = from == 0 && visible as usize == rendered.height();
            let cropped;
            let escape = if whole {
                let Some(escape) = rendered.escape() else {
                    continue;
                };
                escape
            } else {
                let Some(crate::image::PixelInfo::Kitty { png, source, id }) = rendered.pixel()
                else {
                    continue;
                };
                cropped = crate::image::kitty::place(
                    png,
                    rendered.width() as u16,
                    rendered.height() as u16,
                    *id,
                    from,
                    visible,
                    *source,
                );
                &cropped
            };
            queue!(
                out,
                SavePosition,
                MoveTo(placement.column, placement.row),
                Print(escape),
                RestorePosition
            )?;
            continue;
        }

        // Sixel pixels live on the screen itself, so the picture is written:
        // whole when the view has all of it, and only the bands the rows it
        // can see cover when it does not.
        if from == 0 && visible as usize == rendered.height() {
            let Some(escape) = rendered.escape() else {
                continue;
            };
            queue!(
                out,
                SavePosition,
                MoveTo(placement.column, placement.row),
                Print(escape),
                RestorePosition
            )?;
        } else if let Some((header, bands)) = sixel_crop(rendered, from, visible) {
            queue!(
                out,
                SavePosition,
                MoveTo(placement.column, placement.row),
                Print(header)
            )?;
            for band in bands {
                queue!(out, Print(band))?;
            }
            queue!(out, Print("\x1b\\"), RestorePosition)?;
        }
    }
    out.flush()?;
    screen.placements = now;
    Ok(())
}

/// The palette header and the bands a Sixel picture is written in when the
/// view shows only part of it, or nothing when this picture has no pixels
/// for it: the block backend clips with its cells, and a kitty picture is
/// placed by id instead.
fn sixel_crop(
    rendered: &crate::image::Rendered,
    from: u16,
    visible: u16,
) -> Option<(&str, &[String])> {
    let crate::image::PixelInfo::Sixel {
        source, header, bands
    } = rendered.pixel()?
    else {
        return None;
    };
    let range = sixel_bands(from, visible, rendered.height() as u16, source.1, bands.len());
    Some((header.as_str(), &bands[range]))
}

/// Which bands of a Sixel picture a crop covers.
///
/// A band is six pixels tall, so the rows become pixels on the picture's own
/// scale first and the range is rounded out to whole bands: a band cut in
/// half would lose the colours drawn after the cut.
fn sixel_bands(
    from: u16,
    visible: u16,
    rows: u16,
    source_h: u32,
    count: usize,
) -> std::ops::Range<usize> {
    let rows = rows.max(1) as u32;
    let top = from as u32 * source_h / rows / 6;
    let end = (from as u32 + visible as u32) * source_h / rows;
    (top as usize)..(end.div_ceil(6) as usize).min(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{Backend, CellSize, PixelInfo, Rendered, Role};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::KeyCode;
    use ratatui::style::{Modifier, Style};
    use unicode_width::UnicodeWidthChar;

    /// A picture rendered for kitty the way `render` does it: a PNG sent at
    /// the pixels it ships with, measured into a 32x16 cell box.
    fn kitty_picture() -> Rendered {
        let png = crate::testkit::png(64, 64, [200, 100, 50, 255]);
        crate::image::render(
            &png,
            40,
            20,
            Role::Fill,
            Backend::Kitty,
            7,
            CellSize::default(),
        )
        .unwrap()
    }

    #[test]
    fn the_render_carries_the_payload_and_a_crop_rebuilds_from_it() {
        // The whole picture travels in one escape with its payload. When the
        // view shows only its middle rows, the frame rebuilds a placement
        // from the cached PNG — no decode, no second read — and names the
        // source rows that slice covers.
        let rendered = kitty_picture();
        let whole = rendered.escape().expect("the whole placement");
        assert!(whole.contains("i=7,f=100,a=T,c=32,r=16"), "{whole}");
        assert!(!whole.contains("y="), "the whole picture names no crop");
        let (png, source) = match rendered.pixel() {
            Some(PixelInfo::Kitty { png, source, .. }) => (png, *source),
            other => panic!("not a kitty picture: {other:?}"),
        };
        assert_eq!(source, (256, 256), "the crop is measured in the payload's pixels");
        let cropped = crate::image::kitty::place(
            png,
            rendered.width() as u16,
            rendered.height() as u16,
            7,
            4,
            8,
            source,
        );
        assert!(cropped.contains("c=32,r=8"), "{cropped}");
        assert!(cropped.contains("y=64,w=256,h=128"), "{cropped}");
    }

    #[test]
    fn a_sixel_crop_falls_on_whole_bands() {
        // Six rows to a band. A picture 96 pixels tall over six cell rows is
        // sixteen pixels to a row and sixteen bands: the rows turn into
        // pixels on the picture's own scale, and the range is rounded out to
        // whole bands so none is cut through.
        assert_eq!(sixel_bands(0, 6, 6, 96, 16), 0..16, "the whole picture");
        assert_eq!(
            sixel_bands(0, 1, 6, 96, 16),
            0..3,
            "the first row spans three bands"
        );
        assert_eq!(sixel_bands(1, 2, 6, 96, 16), 2..8, "rows one and two");
        assert_eq!(sixel_bands(5, 1, 6, 96, 16), 13..16, "the last row");
        assert_eq!(
            sixel_bands(0, 6, 6, 96, 3),
            0..3,
            "never past the bands that exist"
        );
    }

    /// The screen a scroll is read on, and the number of cells a notch is
    /// allowed to cost: a row of it at most, plus the status row.
    const WIDTH: u16 = 50;
    const HEIGHT: u16 = 24;

    /// One frame of the shape the reader draws: the text rows of
    /// `ui::split_frame`, each a line of its own, and the status row under
    /// them.
    ///
    /// `line` is the first line on the page — moving it moves the page —
    /// `status` is what the status row says, which changes while the text does
    /// not, and `dimmed` is a line some focus has dimmed, which moves with the
    /// line the way a highlight on screen does.
    fn page(
        line: usize,
        status: usize,
        dimmed: Option<usize>,
    ) -> impl FnOnce(&mut ratatui::Frame) -> Vec<ui::Placement> {
        move |frame| {
            let area = frame.area();
            let (text, status_row) = ui::split_frame(area);
            for y in 0..text.height {
                let here = line + y as usize;
                let style = if dimmed == Some(here) {
                    Style::default().add_modifier(Modifier::DIM)
                } else {
                    Style::default()
                };
                write_row(frame, text.x, text.y + y, &sentence(here), style);
            }
            if status_row.height > 0 {
                write_row(
                    frame,
                    status_row.x,
                    status_row.y,
                    &format!("第{status}行 · 位置{status}%"),
                    Style::default(),
                );
            }
            Vec::new()
        }
    }

    /// A line no other line looks like: a shift only lines rows up with the
    /// rows they came from if the rows can be told apart, and moving the page
    /// has to change most of the screen the way a real page's prose does.
    fn sentence(index: usize) -> String {
        const WORDS: [&str; 8] = ["春风", "夏雨", "秋叶", "冬雪", "山川", "河流", "星辰", "大海"];
        let mut line = format!("第{index:04}行 ");
        for step in 0..9 {
            line.push_str(WORDS[(index + step * 3) % WORDS.len()]);
        }
        line
    }

    /// Writes a line the way a widget does: a wide character holds the column
    /// after its own, which is left as the buffer's empty cell.
    fn write_row(frame: &mut ratatui::Frame, x: u16, y: u16, text: &str, style: Style) {
        let right = frame.area().right();
        let mut column = x;
        for c in text.chars() {
            if column >= right {
                break;
            }
            let width = UnicodeWidthChar::width(c).unwrap_or(1).max(1) as u16;
            frame.buffer_mut()[(column, y)]
                .set_symbol(&c.to_string())
                .set_style(style);
            for extra in 1..width {
                if column + extra < right {
                    frame.buffer_mut()[(column + extra, y)].reset();
                }
            }
            column += width;
        }
    }

    /// A screen rendered from scratch — what the terminal would show if the
    /// frame had been drawn on its own, which is what every scroll has to
    /// leave behind it, cell for cell.
    fn fresh(
        render: impl FnOnce(&mut ratatui::Frame) -> Vec<ui::Placement>,
        width: u16,
        height: u16,
    ) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                render(frame);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    /// The presenter on a terminal of the size every case here reads at.
    fn reading() -> (Terminal<TestBackend>, Presenter) {
        (
            Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap(),
            Presenter::default(),
        )
    }

    #[test]
    fn a_page_that_moved_a_few_rows_is_scrolled_and_not_redrawn() {
        // A held scroll key moves the page a row at a time, and one moved row
        // changes every cell: writing the diff is writing the whole screen.
        // The scroll region moves the rows instead and writes the ones the
        // move exposed — a row each at most, plus the status — and the screen
        // has to end up exactly where a redraw would have left it.
        for moved in 1..=3u16 {
            let (mut terminal, mut presenter) = reading();
            presenter
                .present(&mut terminal, true, page(0, 0, None))
                .unwrap();
            assert!(!presenter.scrolled, "the first frame is drawn in full");

            presenter
                .present(&mut terminal, true, page(moved as usize, moved as usize, None))
                .unwrap();
            assert!(presenter.scrolled, "{moved} rows moved, so the screen scrolls");
            assert!(
                presenter.sent <= (WIDTH * moved + WIDTH) as usize,
                "a shift of {moved} rows sent {} cells, more than a row each",
                presenter.sent
            );
            assert_eq!(
                *terminal.backend().buffer(),
                fresh(page(moved as usize, moved as usize, None), WIDTH, HEIGHT),
                "the screen is what a redraw would have left"
            );
        }
    }

    #[test]
    fn a_page_moved_back_down_is_scrolled_the_other_way() {
        // Going back up a page is the same shift read the other way: the rows
        // at the top are the ones the terminal fills in and this writes.
        let (mut terminal, mut presenter) = reading();
        presenter
            .present(&mut terminal, true, page(5, 5, None))
            .unwrap();
        presenter
            .present(&mut terminal, true, page(3, 3, None))
            .unwrap();
        assert!(presenter.scrolled, "two rows came back, which is a scroll down");
        assert!(
            presenter.sent <= (WIDTH * 2 + WIDTH) as usize,
            "a scroll down of two rows sent {} cells",
            presenter.sent
        );
        assert_eq!(
            *terminal.backend().buffer(),
            fresh(page(3, 3, None), WIDTH, HEIGHT)
        );
    }

    #[test]
    fn a_frame_whose_focus_dimmed_a_line_is_redrawn() {
        // The text has not moved: one line's style changed, and no scroll
        // puts a style where it was not. The ordinary diff writes it — and
        // keeps writing the frames that follow, since both paths remember
        // what they showed.
        let (mut terminal, mut presenter) = reading();
        presenter
            .present(&mut terminal, true, page(0, 0, None))
            .unwrap();
        presenter
            .present(&mut terminal, true, page(1, 1, None))
            .unwrap();
        assert!(presenter.scrolled, "the page moved before the focus did");

        presenter
            .present(&mut terminal, true, page(1, 1, Some(6)))
            .unwrap();
        assert!(!presenter.scrolled, "a dimmed line is a redraw, not a scroll");
        assert_eq!(
            *terminal.backend().buffer(),
            fresh(page(1, 1, Some(6)), WIDTH, HEIGHT)
        );

        presenter
            .present(&mut terminal, true, page(2, 2, Some(6)))
            .unwrap();
        assert!(presenter.scrolled, "the frame after a redraw still scrolls");
        assert_eq!(
            *terminal.backend().buffer(),
            fresh(page(2, 2, Some(6)), WIDTH, HEIGHT)
        );
    }

    #[test]
    fn a_status_row_that_changed_alone_is_written_without_a_scroll() {
        // Nothing moved in rows, so there is nothing for a scroll region to
        // do: the few cells that differ are written and the page is left
        // where it is.
        let (mut terminal, mut presenter) = reading();
        presenter
            .present(&mut terminal, true, page(0, 0, None))
            .unwrap();
        presenter
            .present(&mut terminal, true, page(0, 7, None))
            .unwrap();
        assert!(!presenter.scrolled, "a status row moves no rows");
        assert_eq!(
            *terminal.backend().buffer(),
            fresh(page(0, 7, None), WIDTH, HEIGHT)
        );
    }

    /// A page whose rows all read the same, and a status row of its own: any
    /// shift fits it, which is exactly why a shift must be counted first.
    fn uniform(status: usize) -> impl FnOnce(&mut ratatui::Frame) -> Vec<ui::Placement> {
        move |frame| {
            let area = frame.area();
            let (text, status_row) = ui::split_frame(area);
            for y in 0..text.height {
                write_row(frame, text.x, text.y + y, "同一行 同一行 同一行", Style::default());
            }
            if status_row.height > 0 {
                write_row(
                    frame,
                    status_row.x,
                    status_row.y,
                    &format!("位置{status}%"),
                    Style::default(),
                );
            }
            Vec::new()
        }
    }

    #[test]
    fn a_shift_that_would_cost_a_redraw_is_redrawn_instead() {
        // Every row of this page reads the same, so any shift lines every
        // overlapping row up and the shift is real as far as exactness goes.
        // Writing the rows it exposes would still cost a screenful to save a
        // status row's worth of cells, so the frame is redrawn — the escape is
        // only ever sent for a frame it pays for.
        let (mut terminal, mut presenter) = reading();
        presenter
            .present(&mut terminal, true, uniform(0))
            .unwrap();
        presenter
            .present(&mut terminal, true, uniform(7))
            .unwrap();
        assert!(!presenter.scrolled, "a shift nothing differs by writes nothing");
        assert_eq!(
            *terminal.backend().buffer(),
            fresh(uniform(7), WIDTH, HEIGHT)
        );
    }

    #[test]
    fn a_resize_starts_over_from_a_full_draw() {
        // The shadow describes a screen that no longer exists at the new size,
        // so the next frame is drawn the ordinary way — and the frames after
        // it scroll again, against the screen that draw left.
        let (mut terminal, mut presenter) = reading();
        presenter
            .present(&mut terminal, true, page(0, 0, None))
            .unwrap();
        presenter
            .present(&mut terminal, true, page(1, 1, None))
            .unwrap();
        assert!(presenter.scrolled);

        terminal.backend_mut().resize(60, 30);
        presenter
            .present(&mut terminal, true, page(2, 2, None))
            .unwrap();
        assert!(!presenter.scrolled, "a resized screen is drawn in full");
        assert_eq!(
            *terminal.backend().buffer(),
            fresh(page(2, 2, None), 60, 30)
        );

        presenter
            .present(&mut terminal, true, page(3, 3, None))
            .unwrap();
        assert!(presenter.scrolled, "the frame after a resize still scrolls");
        assert_eq!(
            *terminal.backend().buffer(),
            fresh(page(3, 3, None), 60, 30)
        );
    }

    #[test]
    fn one_row_of_a_real_page_is_a_scroll_not_a_redraw() {
        // What a held scroll key does to a book: the view follows the cursor
        // a row at a time from the foot of the page down, and each of those
        // frames used to cost every cell on the screen.
        let book = crate::testkit::text_book("presenter-page", "Page", 400);
        let (_dir, mut app) = crate::testapp::opened_default("presenter-page", &book);
        let (mut terminal, mut presenter) = reading();
        presenter
            .present(&mut terminal, true, |frame| ui::draw(frame, &mut app))
            .unwrap();

        // Walk the cursor to the foot of the page, where the view starts
        // moving: until then a press only moves a highlight inside it.
        let start = app.scroll();
        let mut presses = 0;
        while app.scroll() == start && presses < 400 {
            app.handle_key(crate::testkit::key(KeyCode::Char('j')));
            presses += 1;
        }
        assert!(app.scroll() > start, "the view started moving");
        presenter
            .present(&mut terminal, true, |frame| ui::draw(frame, &mut app))
            .unwrap();

        // Press on: a press whose rows the view moved is a scroll that writes
        // only the rows it exposed, and the screen ends up exactly where a
        // redraw would have left it after every one of them. Not every press
        // is one — the paragraph the cursor sits in keeps full colour while
        // the rest steps back into the page, so walking into the next
        // paragraph changes those rows' style, which is a redraw.
        let mut scrolled = 0;
        for _ in 0..8 {
            // The presenter is measured a row at a time: a rush would carry
            // the focus paragraph along with the rows, and a focus change is
            // a redraw by design rather than the scroll this pins.
            app.reset_rush();
            let before = app.scroll();
            app.handle_key(crate::testkit::key(KeyCode::Char('j')));
            let moved = app.scroll() - before;
            assert!(moved > 0, "the view keeps moving");
            presenter
                .present(&mut terminal, true, |frame| ui::draw(frame, &mut app))
                .unwrap();
            assert_eq!(
                *terminal.backend().buffer(),
                crate::testapp::frame(&mut app, WIDTH, HEIGHT),
                "the screen is what a redraw would have left"
            );
            if presenter.scrolled {
                scrolled += 1;
                assert!(
                    presenter.sent <= (WIDTH * moved as u16 + WIDTH) as usize,
                    "{} cells for {moved} rows: a row each at most",
                    presenter.sent
                );
            }
        }
        assert!(
            scrolled >= 4,
            "only {scrolled} of eight presses scrolled: the rest are the frames this falls back for"
        );
    }

    #[test]
    fn decoding_waits_out_the_motion_and_then_lets_go() {
        // The settle decision is time arithmetic, so it is walked against a
        // clock the test makes rather than one it waits on: moving now asks
        // for nothing, standing still for the whole window asks for
        // everything, and moving again starts the window over.
        let mut settle = Settle::default();
        let t0 = Instant::now();
        assert!(
            settle.allows(t0),
            "nothing has moved, so a book's first page decodes at once"
        );

        settle.moved(t0);
        assert!(!settle.allows(t0), "moving now means no decodes");
        assert!(
            !settle.allows(t0 + SETTLE - Duration::from_millis(1)),
            "a hair short of the window is still the view moving"
        );
        assert!(
            settle.allows(t0 + SETTLE),
            "the window has run out: decoding may go ahead"
        );

        settle.moved(t0 + SETTLE / 2);
        assert!(
            !settle.allows(t0 + SETTLE),
            "motion in the middle restarts the window"
        );
        assert!(settle.allows(t0 + SETTLE / 2 + SETTLE));
    }
}
