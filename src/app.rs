//! Reader state and key handling.

use crate::doc::{Chapter, Locator};
use crate::epub::Book;
use crate::i18n;
use crate::identity::BookId;
use crate::journal::{BookRecord, Journal, State};
use crate::layout::{self, Index, Line};
use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Reading: the page follows a hidden cursor. `j` and `k` move it.
    Reading,
    /// A cursor sits in the text. `j` and `k` move the cursor.
    Cursor,
    /// A cursor with a selection under it: `y` copies what is marked.
    Vision,
    Contents,
    /// The key bindings.
    Help,
    /// Typing a search term.
    Search,
}

/// A position in the laid-out text: a line and a character within it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Cursor {
    line: usize,
    column: usize,
}

/// A place to come back to.
///
/// The cursor is the reading position, so one place is all a jump needs: the
/// page is laid out around it again when the reader comes back.
#[derive(Debug, Clone)]
struct Jump {
    place: Locator,
}

/// How a hold hurries: three numbers, and why they are these. **300 ms** is
/// longer than the gap between two repeats of a held key or two notches of a
/// wheel, and shorter than the pause of a person pressing `j` twice on
/// purpose — so only a hold stays in a rush. **Eight** scrolls per step is
/// about a third of a second of holding: a quick reader pressing one line at
/// a time keeps one line per press, and a hold starts hurrying within a
/// beat. **Four** lines is as much text as the eye can carry while it flies;
/// the presenter would drag more rows for free, but a rush that outruns the
/// line being read loses the reader rather than saving them time.
const RUSH_WINDOW: Duration = Duration::from_millis(300);
const RUSH_RUNS_PER_STEP: usize = 8;
const RUSH_MAX_STEP: usize = 4;

/// Consecutive line scrolls on the reading page, so that holding a key or the
/// wheel hurries the page along and letting go starts over.
///
/// The clock is the caller's, which keeps the curve testable without waiting
/// out the window.
#[derive(Default)]
struct ScrollRush {
    /// How many scrolls have gone the same way without a pause.
    runs: usize,
    /// The way the scrolls are going: 1 down, -1 up.
    direction: isize,
    /// When the last scroll happened; `None` while nothing has scrolled.
    last: Option<Instant>,
}

impl ScrollRush {
    /// The lines this scroll is worth: one to open a run or to break it, and
    /// one more for every `RUSH_RUNS_PER_STEP` the hold has made the same way
    /// without a pause, up to `RUSH_MAX_STEP`. A pause past `RUSH_WINDOW` or
    /// a turn back the other way counts as a fresh scroll — the reader asked
    /// for a line, not for the tail of whatever came before.
    fn lines(&mut self, direction: isize, now: Instant) -> usize {
        let holding = self.last.is_some_and(|last| {
            self.direction == direction && now.saturating_duration_since(last) <= RUSH_WINDOW
        });
        self.runs = if holding { self.runs + 1 } else { 0 };
        self.direction = direction;
        self.last = Some(now);
        (1 + self.runs / RUSH_RUNS_PER_STEP).min(RUSH_MAX_STEP)
    }

    /// Ends the hold: a new chapter, or a scroll the text had no room for.
    fn reset(&mut self) {
        self.runs = 0;
        self.last = None;
    }
}

pub struct App {
    book: Book,
    /// The name this book goes by, as the library knows it rather than as the
    /// file spells it: a scan with `--filenames` names a book from its path, and
    /// a name corrected by hand is what everybody is looking for. A reader with
    /// this book open is looking at the one the shelf listed, and the two have
    /// to agree on what it is called.
    title: String,
    chapter_index: usize,
    chapter: Chapter,
    lines: Vec<Line>,
    /// Index of the topmost visible line.
    scroll: usize,
    /// Width the current layout was built for.
    laid_out_for: u16,
    view_height: u16,
    pub mode: Mode,
    contents_cursor: usize,
    /// The reading position. Hidden while reading, drawn by the modes that
    /// move it, and the one thing the focus, the position and the page all
    /// agree on.
    cursor: Cursor,
    /// True while `g` waits for its second press to make `gg`.
    pending_g: bool,
    status: Option<String>,
    /// True when the reader left the book, either back to the shelf or out of
    /// the program entirely.
    pub should_leave_book: bool,
    /// True when the reader was left for good rather than to pick another book.
    pub should_quit_program: bool,
    id: BookId,
    journal: Journal,
    /// The position the journal gave, waiting for the first layout. The
    /// reader opens on it, centred the way the focus reads it.
    pending_restore: Option<Locator>,
    /// The chapter's pictures: which there are, how much room each takes,
    /// which are decoded for the screen, and how this terminal draws them.
    pictures: crate::pictures::Pictures,
    /// Colours from the active Omarchy theme, as the session's watcher read
    /// them. The watcher itself belongs to the session, not to the book: one
    /// watcher follows the shelf and every book opened from it, and reads the
    /// file once per session rather than once per book.
    theme: crate::theme::Theme,
    search: crate::search::Search,
    /// Where the reader was before following links, most recent last. `Ctrl-o`
    /// walks back out, as Vim's jump list does.
    jumps: Vec<Jump>,
    /// What has been typed into the search prompt so far.
    search_input: String,
    /// Where the contents panel was drawn last, as `(x, y, width, height)`, and
    /// the first entry it showed. Set by the view so a click can name a chapter.
    contents_area: (u16, u16, u16, u16),
    contents_offset: usize,
    /// Where the vision mode began its selection, in text coordinates. The
    /// cursor is the other end; yanking copies inclusively between the two.
    vision_anchor: Option<(usize, usize)>,
    /// Text the vision mode copied, waiting for the caller to hand it to the
    /// clipboard. The reader knows the text, not the terminal.
    clipboard: Option<String>,
    /// The run of consecutive line scrolls on the reading page: a held key or
    /// a spinning wheel speeds up, a pause, a turn or a new chapter starts it
    /// over. The cursor and vision modes never touch it — a selection or a
    /// link under the cursor wants one line at a time.
    rush: ScrollRush,
}

/// The name to show for a book.
///
/// The library's name for it, not the one inside the file. A scan with
/// `--filenames` names a book from its path, and a name somebody corrected by
/// hand is the name everybody is looking for, so the file's own `dc:title` is
/// the last resort rather than the first answer: it is what the shelf would stop
/// showing the moment the book is scanned. A book the library has never been
/// told about — one opened by its path — has nothing else to go by.
fn name_shown(record: Option<&BookRecord>, inside_the_file: &str) -> String {
    match record {
        Some(record) => record.display_title(),
        None => inside_the_file.to_string(),
    }
}

/// The rows of the viewport the reader keeps for itself: the status line.
const STATUS_ROWS: u16 = 1;

/// The viewport rows below the status line — the text area, and therefore
/// also the room a picture may have.
///
/// The status row is the one row a picture never gets; nothing else is held
/// back, where the room used to be four fifths of the view with a floor of
/// four rows. The view splits the frame through `ui::split_frame`, which
/// calls this very function, so the split and the room cannot drift apart: a
/// picture whose room is taller than the area it is drawn in would run under
/// the status line.
pub const fn text_rows(viewport_rows: u16) -> u16 {
    viewport_rows.saturating_sub(STATUS_ROWS)
}

impl App {
    pub fn new(
        mut book: Book,
        id: BookId,
        journal: Journal,
        state: &State,
        cell: crate::image::CellSize,
    ) -> Result<Self> {
        let restore = state.position(&id).cloned();
        let title = name_shown(state.book(&id), book.title());

        // Open the chapter the position points into, not always the first.
        let chapter_index = restore
            .as_ref()
            .and_then(|locator| book.spine.iter().position(|item| item.href == locator.href))
            .unwrap_or(0);
        let chapter = crate::epub::marks::load_chapter(&mut book, chapter_index, cell);

        Ok(Self {
            book,
            title,
            chapter_index,
            chapter,
            lines: Vec::new(),
            scroll: 0,
            laid_out_for: 0,
            view_height: 1,
            mode: Mode::Reading,
            contents_cursor: chapter_index,
            cursor: Cursor::default(),
            pending_g: false,
            status: None,
            should_leave_book: false,
            should_quit_program: false,
            id,
            journal,
            pending_restore: restore,
            pictures: crate::pictures::Pictures::new(cell),
            theme: crate::theme::Theme::default(),
            search: crate::search::Search::default(),
            jumps: Vec::new(),
            search_input: String::new(),
            contents_area: (0, 0, 0, 0),
            contents_offset: 0,
            vision_anchor: None,
            clipboard: None,
            rush: ScrollRush::default(),
        })
    }

    /// Tells the reader how this terminal draws pictures. Set once at startup,
    /// after the terminal has been asked.
    pub fn set_image_backend(&mut self, backend: crate::image::Backend) {
        self.pictures.set_backend(backend);
        self.invalidate_layout();
    }

    pub fn search(&self) -> &crate::search::Search {
        &self.search
    }

    /// The range the vision mode has selected: block and character offsets,
    /// both ends inclusive, in reading order whichever way the cursor was
    /// moved.
    pub fn selection(&self) -> Option<(usize, usize, usize, usize)> {
        if self.mode != Mode::Vision {
            return None;
        }
        let anchor = self.vision_anchor?;
        let cursor = self.cursor_text_position()?;
        let (start, end) = if anchor <= cursor {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        };
        Some((start.0, start.1, end.0, end.1))
    }

    /// The text the vision mode copied, once, for the caller to hand to the
    /// clipboard. The reader knows the text, not the terminal.
    pub fn take_clipboard(&mut self) -> Option<String> {
        self.clipboard.take()
    }

    /// The block the eye is on: the paragraph under the cursor, which is the
    /// reading position in every mode. Since the page is laid out around the
    /// cursor, the first paragraph of a chapter is the focus at its start and
    /// the last one at its end, exactly like every paragraph in between.
    pub fn focused_block(&self) -> Option<usize> {
        self.cursor_text_position().map(|(block, _)| block)
    }
    /// The block of the chapter's own title: the first heading in it, which
    /// the view paints in its own colour rather than in the accent every
    /// heading below it shares.
    pub fn chapter_title_block(&self) -> Option<usize> {
        self.chapter
            .blocks
            .iter()
            .position(|block| matches!(block.kind, crate::doc::BlockKind::Heading(_)))
    }

    /// The search prompt, while one is being typed.
    pub fn search_input(&self) -> Option<&str> {
        if self.mode == Mode::Search {
            Some(&self.search_input)
        } else {
            None
        }
    }

    pub fn theme(&self) -> crate::theme::Theme {
        self.theme
    }

    /// Takes the colours the session's watcher just read, so a theme switch
    /// lands on the frame that follows it rather than on the next book.
    pub fn set_theme(&mut self, theme: crate::theme::Theme) {
        self.theme = theme;
    }

    pub fn image_backend(&self) -> crate::image::Backend {
        self.pictures.backend()
    }

    /// Identifies what is on screen: chapter, scroll position, layout width and
    /// mode.
    ///
    /// Pixel pictures live outside the text buffer, so a partial redraw leaves
    /// them behind. When this token changes, the screen has to be cleared before
    /// the next draw. The mode belongs in it because an overlay covers the text
    /// with cells, which leaves any picture underneath showing through.
    pub fn frame_token(&self) -> (usize, usize, u16, Mode) {
        (
            self.chapter_index,
            self.scroll,
            self.laid_out_for,
            self.mode,
        )
    }

    /// True when a picture needs a pixel protocol to appear.
    pub fn has_pixel_images(&self) -> bool {
        self.pictures.has_pixels()
    }

    /// True while a picture is still being decoded off the draw thread, or
    /// wanted pictures wait only for the view to settle. The session waits on
    /// a short clock rather than on a keypress while this is true, because a
    /// finished picture — and a settled view — has no key to wake the loop
    /// with — and the next one may be a long time coming.
    pub fn pictures_busy(&self) -> bool {
        self.pictures.jobs_outstanding()
    }

    /// Says whether the view has stood still long enough for new decodes to
    /// start. The session sets this once a frame; nothing else ever lowers
    /// it, so a caller that never asks — every test — decodes immediately.
    pub fn set_decode_allowed(&mut self, allowed: bool) {
        self.pictures.set_decode_allowed(allowed);
    }

    // ----- reporting for the view -----

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn chapter_title(&self) -> String {
        self.book.chapter_title(self.chapter_index)
    }

    pub fn chapter_number(&self) -> (usize, usize) {
        (self.chapter_index + 1, self.book.spine.len())
    }

    pub fn contents(&self) -> Vec<String> {
        self.book
            .spine
            .iter()
            .enumerate()
            .map(|(i, _)| self.book.chapter_title(i))
            .collect()
    }

    pub fn contents_cursor(&self) -> usize {
        self.contents_cursor
    }

    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    /// The picture of a block, once rendered.
    pub fn image_at(&self, block: usize) -> Option<&crate::image::Rendered> {
        self.pictures.at(block)
    }

    /// Whether a block's picture is still on its way: its rows are reserved,
    /// and the view holds a placeholder in them until the decoder answers.
    pub fn picture_pending(&self, block: usize) -> bool {
        self.pictures.pending(block)
    }

    /// The words the book gave a block's picture, for a reader who cannot
    /// see it yet.
    pub fn picture_alt(&self, block: usize) -> String {
        self.chapter
            .blocks
            .get(block)
            .map(crate::doc::Block::plain_text)
            .unwrap_or_default()
    }

    /// Decodes wait for a poll, so a test can hold a picture on its way and
    /// bring its answer in by hand.
    #[cfg(test)]
    pub fn defer_picture_jobs(&mut self) {
        self.pictures.defer_jobs();
    }

    /// The link the cursor sits on, if any.
    pub fn link_at_cursor(&self) -> Option<&crate::doc::Link> {
        let (block, offset) = self.cursor_text_position()?;
        self.chapter.link_at(block, offset)
    }

    /// The cursor's screen position, when the mode paints one.
    pub fn cursor_position(&self) -> Option<(usize, usize)> {
        match self.mode {
            Mode::Cursor | Mode::Vision => Some((self.cursor.line, self.cursor.column)),
            _ => None,
        }
    }

    pub fn progress(&self) -> u16 {
        let total = self.lines.len();
        if total <= self.view_height as usize {
            return 100;
        }
        let last = total - self.view_height as usize;
        ((self.scroll.min(last) * 100) / last) as u16
    }

    /// First visible line, needed by the view to map screen rows to lines.
    pub fn scroll(&self) -> usize {
        self.scroll
    }

    pub fn visible_lines(&self) -> &[Line] {
        let end = (self.scroll + self.view_height as usize).min(self.lines.len());
        &self.lines[self.scroll.min(self.lines.len())..end]
    }

    // ----- layout -----

    /// Rebuilds the layout when the view size changed, keeping the reading
    /// position. Called before each draw. `rows` is the text area alone: the
    /// status row is already off, or there was never room for one, and the
    /// view tells the reader how many rows it drew the page in so the room
    /// pictures are measured for and the rows they are drawn in are one number.
    pub fn prepare(&mut self, width: u16, rows: u16) {
        self.view_height = rows.max(1);
        // Pictures that finished decoding off this thread go in before
        // anything is measured: one that failed to draw has its rows measured
        // away by the very build that hears about it, and one that arrived is
        // on the page this frame draws.
        if self.pictures.poll() {
            self.invalidate_layout();
        }
        if width == self.laid_out_for && !self.lines.is_empty() {
            // The cursor keeps the middle of the page, so a height-only change
            // re-centres it; scrolling moved other pictures into view, even
            // though the layout still holds.
            self.follow_cursor();
            self.render_pictures();
            return;
        }
        // Remember text positions, not line numbers: the wrap is about to
        // change. A restored position and the cursor are the same thing now,
        // and the first available one is where the reader stands.
        let place = self
            .pending_restore
            .take()
            .map(|to| (to.block, to.offset))
            .or_else(|| self.cursor_text_position());

        // The room is the whole text area: the viewport with its one status
        // row taken off, which is what `view_height` already counts. The
        // status line is the only row a picture never gets, so a picture may
        // reach the bottom of the view — no four-fifths share, no floor.
        self.pictures
            .measure(&mut self.book, &self.chapter, width, self.view_height);
        let placements = self.pictures.placements();
        self.lines = layout::layout_full(&self.chapter, width, &placements);
        self.laid_out_for = width;

        self.cursor = place
            .and_then(|(block, offset)| self.cursor_at(block, offset))
            .or_else(|| self.first_cursor())
            .unwrap_or_default();
        // The page is read around the cursor, so it follows the cursor to the
        // middle rather than letting the cursor wander to an edge.
        self.follow_cursor();
        // Last, because it needs the finished lines and the settled scroll.
        self.render_pictures();
    }

    /// Decodes the pictures the screen needs, and marks the layout stale when
    /// one of them turned out to draw nothing: the rows it was holding go back
    /// to the text on the next build.
    fn render_pictures(&mut self) {
        let broken = self.pictures.render_visible(
            &mut self.book,
            &self.lines,
            self.scroll,
            self.view_height,
        );
        if broken {
            // The next frame's `prepare` runs the layout again and `measure`
            // skips the pictures that failed.
            self.invalidate_layout();
        }
    }

    /// Marks the layout as stale, so the next draw builds it again. Needed when
    /// something the layout was built from has changed, such as how much room a
    /// picture takes. The only place that gives the width up.
    fn invalidate_layout(&mut self) {
        self.laid_out_for = 0;
    }

    /// Stands the reader at `place` — or leaves them where the chapter puts
    /// them, when there is no place to come back to — in `mode`, with the
    /// page laid out around it on the next draw.
    ///
    /// Every jump lands here, so "move and rebuild the page" is written
    /// once: the place waits for the layout, and the stale lines go so no
    /// draw can show the page the reader has left.
    fn goto_place(&mut self, place: Option<Locator>, mode: Mode) {
        if let Some(place) = place {
            self.pending_restore = Some(place);
        }
        self.mode = mode;
        self.invalidate_layout();
        self.lines.clear();
    }

    /// A place in the chapter the reader is on: where a link lands or a
    /// match sits, named the way a jump names it.
    fn locator_at(&self, block: usize, offset: usize) -> Locator {
        Locator {
            href: self.chapter.href.clone(),
            block,
            offset,
        }
    }

    /// The cursor as a place in the text, which is what a jump records.
    fn cursor_locator(&self) -> Locator {
        let (block, offset) = self.cursor_text_position().unwrap_or((0, 0));
        self.locator_at(block, offset)
    }

    /// The cursor as a block and character offset.
    fn cursor_text_position(&self) -> Option<(usize, usize)> {
        let line = self.lines.get(self.cursor.line)?;
        Some((line.block, line.offset + self.cursor.column))
    }

    /// A cursor pointing at a block offset, clamped into the line that holds it.
    fn cursor_at(&self, block: usize, offset: usize) -> Option<Cursor> {
        let line_index = Index::new(&self.lines).line_of(block, offset)?;
        let line = self.lines.get(line_index)?;
        let column = offset
            .saturating_sub(line.offset)
            .min(line.text_len().saturating_sub(1));
        Some(Cursor {
            line: line_index,
            column,
        })
    }

    fn max_scroll(&self) -> usize {
        self.lines.len().saturating_sub(self.view_height as usize)
    }

    fn clamp_scroll(&mut self) {
        self.scroll = self.scroll.min(self.max_scroll());
    }

    /// Scrolls the page so the cursor stays in the middle of it.
    ///
    /// The cursor is the reading position, so the page follows it rather than
    /// waiting for it to reach an edge: where the cursor goes, the middle of
    /// the page goes, and the text above and below stays even. Near either end
    /// of the chapter the page stops against its edge and the cursor walks the
    /// rest of the way to the first or last line.
    fn follow_cursor(&mut self) {
        self.scroll = self
            .cursor
            .line
            .saturating_sub(self.view_height as usize / 2);
        self.clamp_scroll();
    }

    // ----- keys -----

    pub fn handle_key(&mut self, key: KeyEvent) {
        // `g` waits for its second press; anything else lets go of the
        // sequence and is pressed like the key it is.
        if std::mem::take(&mut self.pending_g) && self.handle_sequence(key) {
            return;
        }
        match self.mode {
            Mode::Reading => self.handle_reading_key(key),
            Mode::Cursor => self.handle_cursor_key(key),
            Mode::Vision => self.handle_vision_key(key),
            Mode::Contents => self.handle_contents_key(key),
            Mode::Search => self.handle_search_key(key),
            // Any key closes the help; there is nothing to do in it.
            Mode::Help => {
                self.mode = Mode::Reading;
                self.status = None;
            }
        }
    }

    /// The second press of `g`: it goes to the start of the chapter, or of
    /// the contents where the same two presses mean the same. Any other key
    /// drops the sequence and keeps its own meaning.
    fn handle_sequence(&mut self, key: KeyEvent) -> bool {
        if key.code != KeyCode::Char('g') {
            return false;
        }
        match self.mode {
            Mode::Contents => self.contents_cursor = 0,
            Mode::Help | Mode::Search => {}
            Mode::Reading | Mode::Cursor | Mode::Vision => self.move_cursor_to_edge(false),
        }
        true
    }

    fn handle_reading_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let page = (self.view_height.saturating_sub(2)).max(1) as isize;

        match key.code {
            // Only `q` quits. Esc takes back what is in force, which is what a
            // Vim user expects, and it must never end the session by accident.
            // `q` leaves the book. Coming from the library that means going
            // back to it; started with a file it means leaving.
            KeyCode::Char('q') => self.should_leave_book = true,
            KeyCode::Esc => self.dismiss(),
            // Scrolling is cursor movement: the page follows the cursor, so
            // one notch takes the reader a line further down the text. Held
            // down, the repeats hurry each other along — one line a
            // key-repeat is a crawl on a slow link.
            KeyCode::Char('j') | KeyCode::Down => self.rush_line(1, Instant::now()),
            KeyCode::Char('k') | KeyCode::Up => self.rush_line(-1, Instant::now()),
            KeyCode::Char('f') if ctrl => self.move_cursor_vertically(page),
            KeyCode::Char('b') if ctrl => self.move_cursor_vertically(-page),
            KeyCode::Char(' ') | KeyCode::PageDown => self.move_cursor_vertically(page),
            KeyCode::Backspace | KeyCode::PageUp => self.move_cursor_vertically(-page),
            KeyCode::Char('L') | KeyCode::Char(']') | KeyCode::Right => self.next_chapter(),
            KeyCode::Char('H') | KeyCode::Char('[') | KeyCode::Left => self.previous_chapter(),
            KeyCode::Char('t') | KeyCode::Tab => self.open_contents(),
            // Entering cursor mode puts a cursor into the text; `v` starts a
            // selection where the eye already is, at the top of the page.
            KeyCode::Char('i') => self.enter_normal_mode(),
            KeyCode::Char('v') => self.start_vision(),
            // What the reading page keeps for itself stops here; every other
            // key is answered the way the modes with a cursor answer it.
            _ => {
                self.movement_key(key);
            }
        }
    }

    fn handle_cursor_key(&mut self, key: KeyEvent) {
        match key.code {
            // Esc lets go of the cursor, and so do `q` and `i` — on the
            // reading page `q` still leaves the book.
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('i') => self.leave_normal_mode(),
            // Enter follows the link under the cursor; Ctrl-o walks back out,
            // as it does in Vim, and is shared with the reading page.
            KeyCode::Enter => self.follow_link(),
            KeyCode::Char('v') => self.start_vision(),
            KeyCode::Char('t') | KeyCode::Tab => self.open_contents(),
            _ => {
                self.movement_key(key);
            }
        }
    }

    /// Collects a selection the way Vim's visual mode does: the movement keys
    /// of the cursor, `y` to copy what is marked, `Esc` or `v` to let it go.
    fn handle_vision_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') => self.yank_selection(),
            KeyCode::Esc | KeyCode::Char('v') => self.leave_vision(),
            _ => {
                self.movement_key(key);
            }
        }
    }

    /// The keys every mode with the text on screen answers alike, once each
    /// mode's own arms have had their say: moving the cursor, stepping
    /// through matches, `?` for help, `Q` for the door.
    ///
    /// What one mode binds and another never did stays that way, so this
    /// declines those keys exactly as those modes always did — a key nobody
    /// bound still does nothing. Answers whether the key was one of them.
    fn movement_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // Half a page is half the view, never less than the one line even a
        // screen too small to halve still has to give.
        let half = (self.view_height / 2).max(1) as isize;
        // The reading page never bound `h l w b 0 $`, and its arrows hop
        // chapters in the arm above; only the modes that draw a cursor move
        // with these.
        let in_text = matches!(self.mode, Mode::Cursor | Mode::Vision);
        // A selection is busy marking text: it never answered the search
        // steps or the walk out of links, and does not start now.
        let selecting = self.mode == Mode::Vision;
        match key.code {
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('Q') => {
                self.should_leave_book = true;
                self.should_quit_program = true;
            }
            KeyCode::Char('j') | KeyCode::Down => self.move_cursor_vertically(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_cursor_vertically(-1),
            KeyCode::Char('d') if ctrl => self.move_cursor_vertically(half),
            KeyCode::Char('u') if ctrl => self.move_cursor_vertically(-half),
            KeyCode::Char('g') => self.pending_g = true,
            KeyCode::Char('G') => self.move_cursor_to_edge(true),
            KeyCode::Char('h') if in_text => self.move_cursor_horizontally(-1),
            KeyCode::Char('l') if in_text => self.move_cursor_horizontally(1),
            KeyCode::Left if in_text => self.move_cursor_horizontally(-1),
            KeyCode::Right if in_text => self.move_cursor_horizontally(1),
            KeyCode::Char('w') if in_text => self.move_cursor_word(true),
            KeyCode::Char('b') if in_text => self.move_cursor_word(false),
            KeyCode::Char('0') if in_text => self.move_cursor_to_line_edge(false),
            KeyCode::Char('$') if in_text => self.move_cursor_to_line_edge(true),
            // The search steps and Ctrl-o — back out of followed links, as
            // Vim does it — belong to the reading page and the cursor alike.
            KeyCode::Char('n') if !selecting => self.jump_to_match(true),
            KeyCode::Char('N') if !selecting => self.jump_to_match(false),
            KeyCode::Char('/') if !selecting => self.open_search(),
            KeyCode::Char('o') if ctrl && !selecting => self.jump_back(),
            _ => return false,
        }
        true
    }

    /// Starts a selection at the cursor, entering cursor mode first when there
    /// is none.
    fn start_vision(&mut self) {
        self.vision_anchor = self.cursor_text_position();
        if self.vision_anchor.is_none() {
            return;
        }
        self.mode = Mode::Vision;
        self.status = Some(i18n::t("vision mode: move, y copies, Esc leaves").into());
    }

    /// Gives up the selection and returns to the cursor.
    fn leave_vision(&mut self) {
        self.mode = Mode::Cursor;
        self.vision_anchor = None;
        self.status = None;
    }

    /// Copies the selected text and leaves the selection.
    fn yank_selection(&mut self) {
        let Some(text) = self.selected_text() else {
            self.leave_vision();
            self.status = Some(i18n::t("nothing to copy").into());
            return;
        };
        let count = text.chars().count();
        self.clipboard = Some(text);
        self.leave_vision();
        self.status = Some(i18n::fill("copied {} characters", &[&count]));
    }

    /// The text between the ends of the selection, both included. Blocks are
    /// joined with a newline, the way a paragraph break pastes.
    fn selected_text(&self) -> Option<String> {
        let anchor = self.vision_anchor?;
        let cursor = self.cursor_text_position()?;
        let (start, end) = if anchor <= cursor {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        };
        let mut out = String::new();
        for (index, block) in self.chapter.blocks.iter().enumerate() {
            if index < start.0 {
                continue;
            }
            if index > end.0 {
                break;
            }
            let text: Vec<char> = block.plain_text().chars().collect();
            let from = if index == start.0 {
                start.1.min(text.len())
            } else {
                0
            };
            let to = if index == end.0 {
                (end.1 + 1).min(text.len())
            } else {
                text.len()
            };
            if from >= to {
                continue;
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.extend(text[from..to].iter());
        }
        (!out.is_empty()).then_some(out)
    }

    /// Collects the search term. Enter runs it, Esc drops it.
    fn handle_search_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Reading;
                self.search_input.clear();
                self.status = None;
            }
            KeyCode::Enter => {
                let query = std::mem::take(&mut self.search_input);
                self.mode = Mode::Reading;
                if query.is_empty() {
                    self.search.clear();
                    self.status = None;
                    return;
                }
                self.search.set_query(query);
                // Start from where the reader stands, not from the chapter's top.
                let from = self.cursor_text_position().unwrap_or((0, 0));
                self.run_search(Some(from));
            }
            KeyCode::Backspace => {
                self.search_input.pop();
            }
            KeyCode::Char(c) => self.search_input.push(c),
            _ => {}
        }
    }

    fn handle_contents_key(&mut self, key: KeyEvent) {
        let last = self.book.spine.len().saturating_sub(1);
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc | KeyCode::Tab | KeyCode::Char('t') => {
                self.mode = Mode::Reading
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.contents_cursor = (self.contents_cursor + 1).min(last)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.contents_cursor = self.contents_cursor.saturating_sub(1)
            }
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('g') => self.pending_g = true,
            KeyCode::Char('G') => self.contents_cursor = last,
            KeyCode::Enter => {
                let target = self.contents_cursor;
                self.go_to_chapter(target);
                self.mode = Mode::Reading;
            }
            _ => {}
        }
    }

    /// Opens the chapter a click landed on, when the contents are shown.
    pub fn handle_click(&mut self, column: u16, row: u16) {
        if self.mode != Mode::Contents {
            return;
        }
        let Some(index) = clicked_chapter(
            self.contents_area,
            self.contents_offset,
            self.book.spine.len(),
            column,
            row,
        ) else {
            return;
        };
        self.contents_cursor = index;
        self.go_to_chapter(index);
        self.mode = Mode::Reading;
    }

    /// Scrolls the view with the wheel: one row a notch, which moves the
    /// cursor — the page follows it. A wheel spun the same way hurries like a
    /// held key, but only on the reading page: the cursor and the selection
    /// keep one row at a time, since what they mark has to stay exact.
    pub fn handle_scroll(&mut self, up: bool) {
        let step: isize = if up { -1 } else { 1 };
        match self.mode {
            Mode::Reading => self.rush_line(step, Instant::now()),
            Mode::Cursor | Mode::Vision => self.move_cursor_vertically(step),
            Mode::Contents => {
                let last = self.book.spine.len().saturating_sub(1);
                let moved = self.contents_cursor as isize + step;
                self.contents_cursor = moved.clamp(0, last as isize) as usize;
            }
            Mode::Help | Mode::Search => {}
        }
    }

    // ----- cursor movement -----

    fn first_cursor(&self) -> Option<Cursor> {
        Index::new(&self.lines)
            .next_selectable(0)
            .map(|line| Cursor { line, column: 0 })
    }

    fn last_cursor(&self) -> Option<Cursor> {
        let index = Index::new(&self.lines);
        let line = index.previous_selectable(self.lines.len().saturating_sub(1))?;
        let column = self.lines[line].text_len().saturating_sub(1);
        Some(Cursor { line, column })
    }

    fn enter_normal_mode(&mut self) {
        self.mode = Mode::Cursor;
        self.status = Some(i18n::t("cursor mode: Enter follows a link, i leaves").into());
    }

    /// Jumps the cursor to the first or last line of the chapter, and the page
    /// with it.
    fn move_cursor_to_edge(&mut self, end: bool) {
        let found = if end {
            self.last_cursor()
        } else {
            self.first_cursor()
        };
        if let Some(cursor) = found {
            self.cursor = cursor;
        }
        self.follow_cursor();
    }

    fn leave_normal_mode(&mut self) {
        self.mode = Mode::Reading;
        self.status = None;
    }

    fn move_cursor_horizontally(&mut self, delta: isize) {
        let mut cursor = self.cursor;
        let index = Index::new(&self.lines);
        if delta > 0 {
            let len = self.lines[cursor.line].text_len();
            if cursor.column + 1 < len {
                cursor.column += 1;
            } else if let Some(next) = index.next_selectable(cursor.line + 1) {
                cursor = Cursor {
                    line: next,
                    column: 0,
                };
            }
        } else if cursor.column > 0 {
            cursor.column -= 1;
        } else if cursor.line > 0
            && let Some(previous) = index.previous_selectable(cursor.line - 1)
        {
            cursor = Cursor {
                line: previous,
                column: self.lines[previous].text_len().saturating_sub(1),
            };
        }
        self.cursor = cursor;
        self.status = None;
        self.follow_cursor();
    }

    /// One line scroll on the reading page, worth what the hold has built it
    /// up to. Only a scroll the text had room for counts towards the next
    /// one: a key held against the chapter's end would otherwise bank a speed
    /// it never spent, and the first line it could move on would be crossed
    /// at a run.
    fn rush_line(&mut self, direction: isize, now: Instant) {
        let steps = self.rush.lines(direction, now);
        let before = self.cursor.line;
        self.move_cursor_vertically(direction * steps as isize);
        if self.cursor.line == before {
            self.rush.reset();
        }
    }

    /// Starts any rush over, so the next line scroll is one line. For tests
    /// that walk the view to an exact place one press at a time: a rush would
    /// step over the very row they are aiming for.
    #[cfg(test)]
    pub(crate) fn reset_rush(&mut self) {
        self.rush.reset();
    }

    fn move_cursor_vertically(&mut self, delta: isize) {
        let cursor = self.cursor;
        let index = Index::new(&self.lines);
        let mut line = cursor.line;
        let steps = delta.unsigned_abs();
        for _ in 0..steps {
            let next = if delta > 0 {
                index.next_selectable(line + 1)
            } else if line == 0 {
                None
            } else {
                index.previous_selectable(line - 1)
            };
            match next {
                Some(found) => line = found,
                None => break,
            }
        }
        let column = cursor
            .column
            .min(self.lines[line].text_len().saturating_sub(1));
        self.cursor = Cursor { line, column };
        self.status = None;
        self.follow_cursor();
    }

    fn move_cursor_to_line_edge(&mut self, end: bool) {
        let line = self.cursor.line;
        let column = if end {
            self.lines[line].text_len().saturating_sub(1)
        } else {
            0
        };
        self.cursor = Cursor { line, column };
        self.status = None;
    }

    /// Word-wise movement over the block's text, so it crosses line breaks the
    /// way the words run in the source.
    fn move_cursor_word(&mut self, forward: bool) {
        let Some((block, offset)) = self.cursor_text_position() else {
            return;
        };
        let Some(text) = self.chapter.blocks.get(block).map(|b| b.plain_text()) else {
            return;
        };
        let chars: Vec<char> = text.chars().collect();
        let target = if forward {
            next_word_start(&chars, offset)
        } else {
            previous_word_start(&chars, offset)
        };

        match target {
            Some(offset) => {
                if let Some(cursor) = self.cursor_at(block, offset) {
                    self.cursor = cursor;
                }
                self.status = None;
                self.follow_cursor();
            }
            // Past the block's edge: continue in the neighbouring line.
            None => self.move_cursor_vertically(if forward { 1 } else { -1 }),
        }
    }

    // ----- navigation -----

    /// Opens a chapter by its href, for coming in from outside.
    pub fn go_to_href(&mut self, href: &str) {
        match self.find_in_spine(href) {
            Some(index) => self.go_to_chapter(index),
            None => self.status = Some(i18n::fill("chapter not found: {}", &[&href])),
        }
    }

    /// Opens the chapter at this number in the spine, counted from 1, for
    /// coming in from outside with a number rather than an href.
    pub fn go_to_chapter_number(&mut self, number: usize) {
        // The command line counts from one, the spine from zero, and the
        // number was checked against the spine before the screen was taken
        // over — so one less is always a chapter that exists.
        self.go_to_chapter(number - 1);
    }

    /// Runs a search and jumps to the first match, for coming in from a hit found
    /// elsewhere.
    pub fn search_for(&mut self, text: String) {
        self.search.set_query(text);
        self.run_search(None);
    }

    /// Scans this chapter for the query now in force and lands on the first
    /// match: from where the reader stands when `from` says so, from the
    /// chapter's top otherwise — and into the book when this chapter holds
    /// nothing after that point.
    fn run_search(&mut self, from: Option<(usize, usize)>) {
        self.search.scan(&self.chapter);
        let (block, offset) = from.unwrap_or((0, 0));
        if self.search.go_to_first_after(block, offset) {
            self.show_current_match();
        } else {
            // Nothing further on in this chapter, so look on ahead.
            self.jump_to_match(true);
        }
    }

    /// Records where the cursor stands, character for character: the cursor
    /// is the reading position, and a character offset is the one thing that
    /// survives a re-wrap, a resize or a different window.
    pub fn save_position(&mut self) {
        if self.cursor_text_position().is_none() {
            // Nothing has been laid out, so there is no place to record.
            return;
        }
        let locator = self.cursor_locator();
        if let Err(err) = self.journal.record_position(&self.id, &locator) {
            self.status = Some(i18n::fill("cannot save position: {}", &[&err]));
        }
    }

    /// Takes back whatever is in force: the search highlighting first, then any
    /// message. Never quits.
    fn dismiss(&mut self) {
        if self.search.is_active() {
            self.search.clear();
            self.status = Some(i18n::t("search cleared").into());
            return;
        }
        self.status = None;
    }

    /// Follows the link under the cursor.
    ///
    /// A target inside the book is resolved against the chapter it is written in,
    /// and the fragment decides the line: `notes.xhtml#fn20` lands on the
    /// footnote, not at the top of the notes. Where the reader came from is kept,
    /// so `Ctrl-o` walks back.
    fn follow_link(&mut self) {
        let Some(link) = self.link_at_cursor().cloned() else {
            self.status = Some(i18n::t("no link here").into());
            return;
        };
        // A link out of the book is shown rather than opened: this is a reader,
        // not a browser.
        if link.target.contains("://") || link.target.starts_with("mailto:") {
            // A link comes from inside the book, and what the terminal is
            // given is what it obeys: the target is cleaned on the way out.
            self.status = Some(i18n::fill(
                "external link: {}",
                &[&crate::journal::clean(&link.target)],
            ));
            return;
        }

        let (file, fragment) = split_target(&link.target);
        // The cursor is on the link being followed, which is exactly where
        // coming back should put it.
        let here = Jump {
            place: self.cursor_locator(),
        };

        // An empty file part means the same chapter.
        let target_href = if file.is_empty() {
            self.chapter.href.clone()
        } else {
            resolve_href(&self.chapter.href, file)
        };

        let Some(target_index) = self.find_in_spine(&target_href) else {
            self.status = Some(i18n::fill(
                "target is not in the reading order: {}",
                &[&crate::journal::clean(&target_href)],
            ));
            return;
        };

        if target_index != self.chapter_index {
            self.go_to_chapter(target_index);
        }
        // Now that the chapter is loaded, its anchors can place the fragment.
        let landing = fragment
            .and_then(|id| self.chapter.anchors.get(id).copied())
            .unwrap_or((0, 0));

        self.jumps.push(here);
        // Land at the target with a cursor, so the next link is one keypress
        // away and Ctrl-o has something to return to.
        self.goto_place(Some(self.locator_at(landing.0, landing.1)), Mode::Cursor);
        self.status = match fragment {
            Some(id) if self.chapter.anchors.contains_key(id) => None,
            Some(id) => Some(format!("target {id:?} not found, showing the chapter")),
            None => None,
        };
    }

    /// Finds a chapter in the reading order.
    ///
    /// An exact match first. Failing that, the file name alone decides: books
    /// exported from publishing tools often keep links to a directory that no
    /// longer exists, such as `../Text/01.htm` when the file sits beside its
    /// neighbours. Refusing to follow those would make the contents of many books
    /// dead text, and a file name is unique inside a container in practice.
    fn find_in_spine(&self, href: &str) -> Option<usize> {
        if let Some(index) = self.book.spine.iter().position(|item| item.href == href) {
            return Some(index);
        }
        let name = std::path::Path::new(href).file_name()?;
        let matches: Vec<usize> = self
            .book
            .spine
            .iter()
            .enumerate()
            .filter(|(_, item)| std::path::Path::new(&item.href).file_name() == Some(name))
            .map(|(index, _)| index)
            .collect();
        // Only when it is unambiguous: two files of the same name in different
        // directories would be a guess.
        match matches.as_slice() {
            [only] => Some(*only),
            _ => None,
        }
    }

    /// Walks back out of followed links.
    fn jump_back(&mut self) {
        let Some(back) = self.jumps.pop() else {
            self.status = Some(i18n::t("nowhere to go back to").into());
            return;
        };
        if let Some(index) = self.find_in_spine(&back.place.href)
            && index != self.chapter_index {
                // Going back must not push another entry onto the stack.
                let chapter = crate::epub::marks::load_chapter(
                    &mut self.book,
                    index,
                    self.pictures.cell(),
                );
                self.open_chapter(index, chapter, None);
            }
        // Coming back into the text means the cursor is wanted, so the mode
        // follows it rather than dropping to plain reading.
        self.goto_place(Some(back.place), Mode::Cursor);
        self.status = None;
    }

    fn open_search(&mut self) {
        self.search_input = self.search.query().to_string();
        self.mode = Mode::Search;
        self.status = None;
    }

    /// Steps to the next or previous match, crossing chapters when this one holds
    /// no further match.
    ///
    /// Chapters are parsed as they are reached rather than up front, so opening a
    /// book stays instant even in a title with a hundred chapters.
    fn jump_to_match(&mut self, forward: bool) {
        if !self.search.is_active() {
            self.status = Some(i18n::t("nothing searched yet: / starts a search").into());
            return;
        }
        // Within the current chapter first.
        let stepped = if forward {
            self.search.next_in_chapter()
        } else {
            self.search.previous_in_chapter()
        };
        if stepped {
            self.show_current_match();
            return;
        }

        let count = self.book.spine.len();
        let mut index = self.chapter_index;
        for _ in 0..count {
            index = if forward {
                if index + 1 >= count {
                    break;
                }
                index + 1
            } else {
                if index == 0 {
                    break;
                }
                index - 1
            };

            let chapter =
                crate::epub::marks::load_chapter(&mut self.book, index, self.pictures.cell());
            let hits = crate::search::find_all(&chapter, self.search.query());
            if hits.is_empty() {
                continue;
            }
            // Found one: move there and pick the first or last match. The
            // chapter was read through on the way here, so the finds go with
            // it rather than being found again inside.
            self.save_position();
            self.open_chapter(index, chapter, Some(hits));
            if forward {
                self.search.go_to_first();
            } else {
                self.search.go_to_last();
            }
            self.show_current_match();
            return;
        }
        self.status = Some(i18n::fill(
            "no more matches for {}",
            &[&format!("{:?}", self.search.query())],
        ));
    }

    /// Scrolls to the match the reader is on and reports which it is.
    fn show_current_match(&mut self) {
        let Some((block, offset)) = self.search.current() else {
            return;
        };
        // Force a rebuild so the pending position is applied.
        self.goto_place(Some(self.locator_at(block, offset)), Mode::Reading);
        self.status = match self.search.progress() {
            Some((at, total)) => Some(format!("{:?}  {at}/{total}", self.search.query())),
            None => None,
        };
    }

    fn open_contents(&mut self) {
        self.contents_cursor = self.chapter_index;
        self.mode = Mode::Contents;
    }

    /// Tells the reader where the contents panel was drawn and how far it
    /// scrolled, so a click can name a chapter.
    pub fn set_contents_area(&mut self, x: u16, y: u16, width: u16, height: u16, offset: usize) {
        self.contents_area = (x, y, width, height);
        self.contents_offset = offset;
    }

    fn next_chapter(&mut self) {
        if self.chapter_index + 1 < self.book.spine.len() {
            self.go_to_chapter(self.chapter_index + 1);
        } else {
            self.status = Some(i18n::t("end of book").into());
        }
    }

    fn previous_chapter(&mut self) {
        if self.chapter_index > 0 {
            self.go_to_chapter(self.chapter_index - 1);
        } else {
            self.status = Some(i18n::t("start of book").into());
        }
    }

    fn go_to_chapter(&mut self, index: usize) {
        if index >= self.book.spine.len() {
            return;
        }
        self.save_position();
        let chapter = crate::epub::marks::load_chapter(&mut self.book, index, self.pictures.cell());
        self.open_chapter(index, chapter, None);
        if self.mode == Mode::Cursor {
            self.mode = Mode::Reading;
        }
        self.status = None;
    }

    /// Installs the chapter at `index`, already parsed as `chapter`, and forgets
    /// the last chapter's pictures. A standing search is re-run over the new
    /// text — unless the caller brings the matches it already found, since `n`
    /// has to read a chapter before opening it and must not pay twice. The
    /// cursor goes back to the chapter's first line, and the layout is marked
    /// stale so the next draw rebuilds it. Where to stand — its top or a
    /// restored position — and which mode to stand in are the callers'.
    fn open_chapter(
        &mut self,
        index: usize,
        chapter: Chapter,
        hits: Option<Vec<crate::search::Hit>>,
    ) {
        self.chapter_index = index;
        self.chapter = chapter;
        // This chapter's pictures have not been tried yet.
        self.pictures.forget_chapter();
        match hits {
            Some(hits) => self.search.adopt(hits),
            None if self.search.is_active() => self.search.scan(&self.chapter),
            None => {}
        }
        self.contents_cursor = index;
        self.cursor = Cursor::default();
        // The hold that walked into this chapter means nothing in it: the
        // first scroll here is one line, whatever the last chapter's keys had
        // built up.
        self.rush.reset();
        // A chapter on its own moves nothing else: the mode and the place to
        // stand are the callers' to say.
        self.goto_place(None, self.mode);
    }
}

/// The key bindings, grouped for the help. Kept beside the handlers above so a
/// changed key does not leave a stale description behind.
pub fn bindings() -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
    let t = i18n::t;
    vec![
        (
            "Reading",
            vec![
                ("j k ↓ ↑", t("line down, up")),
                ("Space Backspace", t("page down, up")),
                ("Ctrl-d Ctrl-u", t("half a page")),
                ("gg G", t("start, end of chapter")),
                ("L ] →", t("next chapter")),
                ("H [ ←", t("previous chapter")),
                ("t Tab", t("contents")),
                ("/", t("search the book")),
                ("n N", t("next, previous match")),
                ("i", t("cursor mode, with a cursor in the text")),
                ("v", t("start a selection")),
                ("Esc", t("clear the search")),
                ("q", t("back to the library")),
                ("Q", t("quit")),
            ],
        ),
        (
            "Cursor",
            vec![
                ("h l ← → w b 0 $", t("move by character, word, to line edge")),
                ("j k ↑ ↓ gg G", t("move by line, to chapter edges")),
                ("Enter", t("follow the link under the cursor")),
                ("Ctrl-o", t("back out of followed links")),
                ("/ n N", t("search, next match, previous match")),
                ("Esc i", t("leave the cursor")),
            ],
        ),
        (
            "Vision",
            vec![
                ("h l j k w b 0 $", t("move the cursor")),
                ("y", t("copy the selection")),
                ("Esc v", t("leave vision")),
            ],
        ),
        (
            "Contents",
            vec![
                ("j k ↑ ↓ gg G", t("move the cursor")),
                ("Enter", t("open the chapter")),
                ("q Esc", t("close")),
            ],
        ),
    ]
}

/// Turns a click inside the contents panel into a chapter index.
///
/// `area` is the panel as drawn, `(x, y, width, height)`; `offset` is the first
/// entry it showed, which only the view knows because the list scrolled itself
/// to keep the cursor visible; `count` is how many chapters there are. `None`
/// means the click landed on the border, outside the panel, or past the last
/// chapter.
fn clicked_chapter(
    area: (u16, u16, u16, u16),
    offset: usize,
    count: usize,
    column: u16,
    row: u16,
) -> Option<usize> {
    let (x, y, width, height) = area;
    // Borders take the first and last column and row, so a panel narrower than
    // three cells has no rows at all.
    if width < 3 || height < 3 {
        return None;
    }
    let (left, top) = (x + 1, y + 1);
    let (right, bottom) = (x + width - 1, y + height - 1);
    if column < left || column >= right || row < top || row >= bottom {
        return None;
    }
    let index = offset + (row - top) as usize;
    (index < count).then_some(index)
}

/// Splits a link target into its file part and its fragment.
fn split_target(target: &str) -> (&str, Option<&str>) {
    match target.split_once('#') {
        Some((file, "")) => (file, None),
        Some((file, fragment)) => (file, Some(fragment)),
        None => (target, None),
    }
}

/// Resolves a link's file part against the chapter it appears in.
fn resolve_href(from: &str, target: &str) -> String {
    let base = std::path::Path::new(from)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    crate::epub::container_path(&base, target)
}

/// Start of the next word at or after `from`, if one exists in this block.
fn next_word_start(chars: &[char], from: usize) -> Option<usize> {
    let mut at = from;
    // Leave the current word.
    while at < chars.len() && !chars[at].is_whitespace() {
        at += 1;
    }
    // Skip the gap.
    while at < chars.len() && chars[at].is_whitespace() {
        at += 1;
    }
    if at < chars.len() { Some(at) } else { None }
}

/// Start of the word before `from`, if one exists in this block.
fn previous_word_start(chars: &[char], from: usize) -> Option<usize> {
    if from == 0 {
        return None;
    }
    let mut at = from - 1;
    while at > 0 && chars[at].is_whitespace() {
        at -= 1;
    }
    while at > 0 && !chars[at - 1].is_whitespace() {
        at -= 1;
    }
    Some(at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{ctrl, key};

    #[test]
    fn the_reader_calls_a_book_what_the_library_calls_it() {
        // Scanned with --filenames, this book is 1973年的弹子球; the file inside
        // says 一九七三年的弹子球. The shelf shows the first, so a reader with
        // the book open has to show it too, or the two look like two books.
        let mut record =
            crate::journal::BookRecord::new("/books/村上春树/1973年的弹子球.epub".into());
        record.title = Some("1973年的弹子球".into());
        assert_eq!(
            name_shown(Some(&record), "一九七三年的弹子球"),
            "1973年的弹子球"
        );
        // A book the library never saw goes by the name in its file.
        assert_eq!(name_shown(None, "一九七三年的弹子球"), "一九七三年的弹子球");
    }

    #[test]
    fn a_link_target_resolves_against_its_chapter() {
        assert_eq!(
            split_target("ch03.xhtml#fn20"),
            ("ch03.xhtml", Some("fn20"))
        );
        assert_eq!(split_target("#fn20"), ("", Some("fn20")));
        assert_eq!(split_target("ch03.xhtml"), ("ch03.xhtml", None));
        // A trailing hash names no target.
        assert_eq!(split_target("ch03.xhtml#"), ("ch03.xhtml", None));

        assert_eq!(
            resolve_href("OEBPS/ch01.xhtml", "ch02.xhtml"),
            "OEBPS/ch02.xhtml"
        );
        assert_eq!(
            resolve_href("OEBPS/text/ch01.xhtml", "../notes.xhtml"),
            "OEBPS/notes.xhtml"
        );
        assert_eq!(resolve_href("ch01.xhtml", "ch02.xhtml"), "ch02.xhtml");
        // A link target is a URI too, so a chapter whose name is not ASCII is
        // written percent-encoded and has to be decoded before it can be looked
        // up in the reading order.
        assert_eq!(
            resolve_href("OEBPS/ch01.xhtml", "%E5%BC%95%E8%A8%80.xhtml"),
            "OEBPS/引言.xhtml"
        );
    }

    #[test]
    fn the_room_leaves_out_the_status_row() {
        // The view splits the frame the way `ui::draw` does — text area above,
        // one status row below — and `split_frame` builds that split *from*
        // `text_rows`, so the room a picture may have and the rows it is drawn
        // in are one number: every row of the text area and no more, where it
        // used to be four fifths of the viewport with a floor of four.
        let frame = ratatui::layout::Rect::new(0, 0, 80, 40);
        let (text, status) = crate::ui::split_frame(frame);
        assert_eq!(status.height, 1, "the status line is one row");
        assert_eq!(text_rows(frame.height), text.height);
        assert_eq!(text_rows(40), 39, "and the room is what it leaves");
        // A viewport too short to spare a row still leaves the reader a row
        // to read in; `prepare` is where that floor lives.
        assert_eq!(text_rows(1), 0);
    }

    #[test]
    fn word_movement_walks_in_both_directions() {
        let chars: Vec<char> = "one two  three".chars().collect();
        assert_eq!(next_word_start(&chars, 0), Some(4));
        assert_eq!(next_word_start(&chars, 4), Some(9));
        assert_eq!(next_word_start(&chars, 9), None);
        assert_eq!(previous_word_start(&chars, 9), Some(4));
        assert_eq!(previous_word_start(&chars, 4), Some(0));
        assert_eq!(previous_word_start(&chars, 0), None);
    }

    /// `count` scrolls the same way, `gap` apart from `start` — the repeats
    /// of a held key — and what the last one was worth.
    fn held(rush: &mut ScrollRush, start: Instant, count: usize, gap: Duration) -> usize {
        let mut worth = 0;
        for run in 0..count {
            worth = rush.lines(1, start + gap * run as u32);
        }
        worth
    }

    #[test]
    fn a_hold_stays_at_one_line_an_apiece_until_it_has_run_eight() {
        let start = Instant::now();
        let gap = Duration::from_millis(40);
        let mut rush = ScrollRush::default();
        // The first scroll of all is one line...
        assert_eq!(rush.lines(1, start), 1, "the first scroll");
        // ...and seven more repeats of the held key are still one line each:
        // eight rapid scrolls, none of them hurried...
        assert_eq!(
            held(&mut rush, start + gap, 7, gap),
            1,
            "eight rapid scrolls stay one line each"
        );
        // ...and the ninth is where the hold starts paying.
        assert_eq!(rush.lines(1, start + gap * 8), 2, "the ninth hurries");
    }

    #[test]
    fn the_rush_grows_a_line_every_eight_scrolls_and_stops_at_four() {
        let gap = Duration::from_millis(40);
        // How many scrolls of one hold, and what the last was worth: a line
        // more for every eight, and no more than four however long the hold.
        for (count, want) in [(8, 1), (9, 2), (16, 2), (17, 3), (24, 3), (25, 4), (104, 4)] {
            let mut rush = ScrollRush::default();
            assert_eq!(
                held(&mut rush, Instant::now(), count, gap),
                want,
                "{count} rapid scrolls"
            );
        }
    }

    #[test]
    fn a_pause_ends_the_hold() {
        let start = Instant::now();
        let gap = Duration::from_millis(40);
        let mut rush = ScrollRush::default();
        assert_eq!(held(&mut rush, start, 9, gap), 2, "the hold is hurrying");
        // The window measured from the last scroll, just past it: one line,
        // and the run counted from nothing again.
        let after = start + gap * 8 + RUSH_WINDOW + Duration::from_millis(1);
        assert_eq!(rush.lines(1, after), 1, "the pause starts the hold over");
    }

    #[test]
    fn turning_round_ends_the_hold() {
        let start = Instant::now();
        let gap = Duration::from_millis(40);
        let mut rush = ScrollRush::default();
        assert_eq!(held(&mut rush, start, 9, gap), 2, "down has got going");
        // Up, inside the window: the run was down's, so this is one line...
        let turn = start + gap * 9;
        assert_eq!(rush.lines(-1, turn), 1, "turning round starts over");
        // ...and up hurries on its own count, not on down's.
        assert_eq!(
            rush.lines(-1, turn + gap),
            1,
            "the new run counts from its own start"
        );
    }

    #[test]
    fn a_click_on_the_contents_names_a_chapter() {
        // A panel of six rows, its rows one cell in from the border.
        let area = (10, 5, 40, 6);
        assert_eq!(clicked_chapter(area, 0, 10, 11, 6), Some(0));
        assert_eq!(clicked_chapter(area, 0, 10, 11, 9), Some(3));
        // A scrolled list shifts the entries, not the rows they sit on.
        assert_eq!(clicked_chapter(area, 7, 10, 11, 6), Some(7));
        // The border, the space outside it, and the row under the last entry.
        assert_eq!(clicked_chapter(area, 0, 10, 10, 6), None);
        assert_eq!(clicked_chapter(area, 0, 10, 11, 5), None);
        assert_eq!(clicked_chapter(area, 0, 10, 11, 10), None);
        assert_eq!(clicked_chapter(area, 0, 10, 9, 6), None);
        // A click past the last chapter names nothing.
        assert_eq!(clicked_chapter(area, 3, 4, 11, 6), Some(3));
        assert_eq!(clicked_chapter(area, 3, 4, 11, 7), None);
    }

    #[test]
    fn the_position_is_the_cursor_and_opens_centred() {
        let body: String = (0..12)
            .map(|i| {
                format!("<p>Paragraph {i} carries enough words to wrap the line once.</p>")
            })
            .collect();
        let path = crate::testkit::TempBook::new("app-position", "Text", &[&body]);
        let (dir, mut app) = crate::testapp::opened_default("app-position-journal", &path);
        let id = BookId::of_file(&path).unwrap();
        app.prepare(40, 9);
        // Read a few screens in, so the cursor stands well inside the chapter.
        for _ in 0..12 {
            app.handle_key(key(KeyCode::Char('j')));
        }
        app.save_position();
        let (block, offset) = app.cursor_text_position().expect("the cursor is in text");

        let state = Journal::replay(&dir).unwrap();
        let position = state.position(&id).expect("the position was written");
        assert_eq!(
            (position.block, position.offset),
            (block, offset),
            "the cursor's own place is what is recorded"
        );
        assert!(block > 0, "which is a place inside the chapter");

        // Opening the book again stands the cursor there and puts the line it
        // sits on in the middle of the page.
        let mut again = App::new(
            Book::open(&path).unwrap(),
            id,
            Journal::open(&dir).unwrap(),
            &state,
            crate::image::CellSize::default(),
        )
        .unwrap();
        again.prepare(40, 9);
        assert_eq!(
            again.cursor_text_position(),
            Some((block, offset)),
            "the cursor comes back to the same character"
        );
        assert_eq!(
            again.scroll() + 4,
            again.cursor.line,
            "and its line is the one the page is read around"
        );
    }

    #[test]
    fn scrolling_walks_the_cursor_through_a_pictures_rows() {
        // The cursor is the reading position and the page follows it, so it
        // has to step through a picture's own lines: if it jumped across them
        // in one move, the picture would never get its chance to appear whole
        // in the middle of the page.
        let path = crate::testkit::TempBook::with(
            "app-walk-picture",
            "Walk",
            &[r#"<p>before the picture</p><img src="pic.png" alt="a diagram"/><p>after the picture</p>"#],
            &[
                (
                    "OEBPS/content.opf",
                    &crate::testkit::opf(
                        "Walk",
                        r#"<item id="pic" href="pic.png" media-type="image/png"/>"#,
                    ),
                ),
                ("OEBPS/pic.png", &crate::testkit::png(200, 120, [10, 20, 30, 255])),
            ],
        );
        let (_dir, mut app) = crate::testapp::opened_default("app-walk-picture-journal", &path);
        app.prepare(40, 13);

        let mut picture_rows = 0;
        for _ in 0..40 {
            app.handle_key(key(KeyCode::Char('j')));
            if matches!(app.lines[app.cursor.line].kind, crate::layout::LineKind::Image { .. }) {
                picture_rows += 1;
            }
        }
        assert!(
            picture_rows >= 3,
            "the cursor crossed the picture in one jump: {picture_rows} row(s)"
        );
    }

    /// How many rows one block is given as a picture. A picture that measured
    /// gets as many as it is tall; without pixels the alt text has to do, and
    /// that is one row.
    fn picture_rows(app: &App, block: usize) -> usize {
        app.lines
            .iter()
            .filter(|line| {
                line.block == block && matches!(line.kind, layout::LineKind::Image { .. })
            })
            .count()
    }

    /// A book whose one drawable picture has a header that measures and bytes
    /// that do not decode — `broken_png` cut it just past the IDAT chunk's own
    /// header — plus a picture that is not in the container at all.
    fn liar_book() -> crate::testkit::TempBook {
        crate::testkit::TempBook::with(
            "app-liar",
            "Liar",
            &[
                r#"<p>before</p><img src="bad.png" alt="liar"/><img src="gone.png" alt="missing"/><p>after</p>"#,
            ],
            &[
                (
                    "OEBPS/content.opf",
                    &crate::testkit::opf(
                        "Liar",
                        r#"<item id="bad" href="bad.png" media-type="image/png"/>
  <item id="gone" href="gone.png" media-type="image/png"/>"#,
                    ),
                ),
                ("OEBPS/bad.png", &crate::testkit::broken_png(100, 100)),
            ],
        )
    }

    /// The frames after a picture's bytes have failed to decode, on either
    /// path the failure can arrive on: the rows the block was holding go back
    /// to the text — the alt text keeps its one — and the chapter settles
    /// there, those bytes never tried again.
    fn decode_failure_settles(app: &mut App, block: usize, width: u16, rows: u16) {
        for _ in 0..2 {
            app.prepare(width, rows);
            assert_eq!(
                picture_rows(app, block),
                1,
                "the rows went back to the text, leaving the alt text"
            );
            assert!(
                app.pictures.unreadable.contains(&block),
                "the chapter does not try those bytes again"
            );
        }
    }

    #[test]
    fn a_picture_that_cannot_be_decoded_reserves_nothing() {
        // The header measures — 100x100 — but the pixel stream was cut, so
        // nothing can be drawn. The block must not go on holding rows for a
        // picture that draws nothing: the failed decode marks it unreadable,
        // the chapter is laid out once more without it, and those bytes are
        // never tried again for this chapter. The picture that is missing
        // from the container entirely never gets a slot in the first place.
        let path = liar_book();
        let (_dir, mut app) = crate::testapp::opened_default("app-liar-journal", &path);

        // First frame: the header is believed and the block gets its rows —
        // only for as long as it takes to find out that the bytes are bad.
        app.prepare(80, 23);
        assert_eq!(
            app.pictures.slots.len(),
            1,
            "the lying header measured; the missing picture never does"
        );
        let block = app.pictures.slots[0].block;
        assert!(
            picture_rows(&app, block) > 1,
            "the layout believed the header and reserved the picture's rows"
        );
        assert!(
            app.pictures.unreadable.contains(&block),
            "the decode failed"
        );

        decode_failure_settles(&mut app, block, 80, 23);
        assert!(
            app.pictures.slots.is_empty(),
            "the block reserves nothing, and the missing picture never did"
        );
    }

    /// A book whose one chapter holds a picture that draws and a picture
    /// whose header measures over bytes cut short just past the IDAT chunk's
    /// own header: the decode, and not the measurement, is what fails.
    fn drawn_and_liar_book() -> crate::testkit::TempBook {
        let png = crate::testkit::png(200, 60, [10, 20, 30, 255]);
        crate::testkit::TempBook::with(
            "app-off-thread",
            "OffThread",
            &[
                r#"<p>before</p><img src="pic.png" alt="a diagram"/><img src="bad.png" alt="liar"/><p>after</p>"#,
            ],
            &[
                (
                    "OEBPS/content.opf",
                    &crate::testkit::opf(
                        "OffThread",
                        r#"<item id="pic" href="pic.png" media-type="image/png"/>
  <item id="bad" href="bad.png" media-type="image/png"/>"#,
                    ),
                ),
                ("OEBPS/pic.png", &png),
                ("OEBPS/bad.png", &crate::testkit::broken_png(200, 60)),
            ],
        )
    }

    #[test]
    fn a_picture_decoded_away_from_the_draw_lands_on_the_next_poll() {
        // Decoding on the draw thread is what stutters, so the bytes are
        // handed over and the picture arrives a frame or two later, into rows
        // the layout had already reserved for it. The jobs wait here instead
        // of running, and the poll that follows brings the answers in by
        // hand: the picture that draws, and the one whose bytes are a lie.
        let path = drawn_and_liar_book();
        let (_dir, mut app) = crate::testapp::opened_default("app-off-thread-journal", &path);
        app.pictures.defer_jobs();

        // The questions go out; nothing is decoded yet.
        app.prepare(40, 20);
        let slot = |name: &str| {
            app.pictures
                .slots
                .iter()
                .find(|slot| slot.src.ends_with(name))
                .unwrap_or_else(|| panic!("{name} was measured"))
                .block
        };
        let (good, bad) = (slot("pic.png"), slot("bad.png"));
        assert!(app.image_at(good).is_none(), "a queued job has not run yet");
        assert!(app.pictures.jobs_outstanding(), "the session keeps a clock running while they are out");
        assert!(app.pictures.unreadable.is_empty(), "nothing has failed while nothing has run");

        // The next poll answers both jobs at once.
        app.prepare(40, 20);
        assert!(app.image_at(good).is_some(), "a queued job installs on the next poll");
        assert!(app.pictures.unreadable.contains(&bad), "the failed job lands where failures land");
        assert!(!app.pictures.jobs_outstanding(), "and no job is left standing");

        decode_failure_settles(&mut app, bad, 40, 20);
        assert_eq!(app.laid_out_for, 40, "the failure was handled once and only once");
    }

    /// Where the cursor stands after the press: where it was, on the line a
    /// landmark key is exact about, or moved — the direction a reader would
    /// say it went in.
    #[derive(Debug, PartialEq)]
    enum Where {
        Stayed,
        Line(usize),
        Forward,
        Back,
    }

    /// What the press said: the status it left standing, nothing at all, or
    /// these very words.
    #[derive(Debug, PartialEq)]
    enum Said {
        Kept,
        Cleared,
        Says(String),
    }

    /// The two ways out of the book.
    #[derive(Debug, PartialEq)]
    enum Out {
        Stay,
        LeaveBook,
        QuitProgram,
    }

    /// What one press must leave behind. `mode` is the mode the reader ends
    /// in, `None` the mode the key was pressed in; `chapter` is the chapter
    /// the reader ends in, `None` the one it started in.
    #[derive(Debug, PartialEq)]
    struct Want {
        mode: Option<Mode>,
        cursor: Where,
        said: Said,
        chapter: Option<usize>,
        out: Out,
    }

    /// The page as it was: the same mode, the cursor where it stood, the
    /// status still up and no sign of leaving — what a key that does nothing
    /// must leave behind, and the start of every other expectation.
    fn same() -> Want {
        Want { mode: None, cursor: Where::Stayed, said: Said::Kept, chapter: None, out: Out::Stay }
    }

    impl Want {
        fn in_mode(mut self, mode: Mode) -> Self { self.mode = Some(mode); self }
        fn line(mut self, line: usize) -> Self { self.cursor = Where::Line(line); self }
        fn forward(mut self) -> Self { self.cursor = Where::Forward; self }
        fn back(mut self) -> Self { self.cursor = Where::Back; self }
        fn cleared(mut self) -> Self { self.said = Said::Cleared; self }
        fn says(mut self, words: impl Into<String>) -> Self {
            self.said = Said::Says(words.into());
            self
        }
        fn in_chapter(mut self, chapter: usize) -> Self { self.chapter = Some(chapter); self }
        fn leaves(mut self) -> Self { self.out = Out::LeaveBook; self }
        fn quits(mut self) -> Self { self.out = Out::QuitProgram; self }
    }

    /// A book with two chapters, a link from the first into the second, and
    /// matches of `cat` in both — every key the table presses has a page to
    /// move, a link to follow or a boundary to step over.
    fn key_book(name: &str) -> crate::testkit::TempBook {
        let first = r#"<p>The dog dozed by the fire all afternoon, one ear twitching at the sounds from the street while the kettle cooled on the stove.</p>
<p>A cat came in from the rain and sat by the door, and neither animal so much as looked at the other while it waited.</p>
<p>Outside a bicycle rattled past and the bird in its cage muttered at the noise, but the room stayed quiet and warm.</p>
<p>The second cat, a grey one, climbed onto the shelf and pushed a book half off the edge without looking down.</p>
<p>Nobody moved to stop it; there was bread on the table and tea in the pot and no reason at all to stand up.</p>
<p><a href="ch1.xhtml">the way on</a></p>
<p>One cat finally stretched, yawned, and went back to sleep by the fire, paws curled under its chin.</p>
<p>The dog opened one eye, decided against the world, and closed it again until supper.</p>
<p>Rain kept at the windows, drawing lines down the glass that nobody counted.</p>
<p>The bird sang a short phrase twice and then nothing more, as if it had forgotten the rest.</p>
<p>Evening came on slowly, and the lamp was lit before anyone noticed the light had gone.</p>"#;
        let second = r#"<p>A cat from the next house crossed the yard without a pause.</p>
<p>By evening two cats and one dog had settled in the doorway.</p>"#;
        crate::testkit::TempBook::new(name, "Keys", &[first, second])
    }

    /// A reader with the search standing on a match in the first chapter, the
    /// cursor on the link to the second, and `mode` in force — where a person
    /// would be when pressing the key under test, reached the way a person
    /// reaches it.
    fn keyed_app(app: &mut App, mode: Mode) {
        app.search_for("cat".into());
        app.prepare(40, 14);
        // On the link between the chapters, so `Enter` has something to follow
        // and every movement key starts from the same place.
        let link = (app.chapter.links[0].block, app.chapter.links[0].start);
        let at = app.cursor_at(link.0, link.1).unwrap();
        app.cursor = at;
        app.follow_cursor();
        match mode {
            Mode::Reading => {}
            Mode::Cursor => app.handle_key(key(KeyCode::Char('i'))),
            Mode::Vision => {
                app.handle_key(key(KeyCode::Char('i')));
                app.handle_key(key(KeyCode::Char('v')));
            }
            _ => panic!("the table only drives the modes with text under a cursor"),
        }
    }

    /// The press a label names, as crossterm sends it.
    fn press(label: &str) -> KeyEvent {
        match label {
            "Esc" => key(KeyCode::Esc),
            "Down" => key(KeyCode::Down),
            "Up" => key(KeyCode::Up),
            "Left" => key(KeyCode::Left),
            "Right" => key(KeyCode::Right),
            "Space" => key(KeyCode::Char(' ')),
            "Tab" => key(KeyCode::Tab),
            "Enter" => key(KeyCode::Enter),
            "Backspace" => key(KeyCode::Backspace),
            "PageDown" => key(KeyCode::PageDown),
            "PageUp" => key(KeyCode::PageUp),
            other => match other.strip_prefix("ctrl+") {
                Some(c) => ctrl(c.chars().next().unwrap()),
                None => key(KeyCode::Char(other.chars().next().unwrap())),
            },
        }
    }

    /// Every key the three text modes answer, dead keys included: a key that
    /// quietly wakes up in the wrong mode is exactly the way deduplicating
    /// the handlers could go wrong.
    fn presses(mode: Mode) -> Vec<(&'static str, KeyEvent)> {
        const SHARED: &[&str] = &["?", "j", "k", "Down", "Up", "ctrl+d", "ctrl+u", "g", "G", "Q"];
        const READING: &[&str] = &[
            "q", "Esc", "ctrl+f", "ctrl+b", "Space", "PageDown", "Backspace", "PageUp", "L", "]",
            "Right", "H", "[", "Left", "t", "Tab", "ctrl+o", "i", "v", "/", "n", "N", "h", "l",
            "w", "b", "0", "$", "Enter", "y",
        ];
        const CURSOR: &[&str] = &[
            "Esc", "q", "i", "v", "Enter", "t", "Tab", "h", "l", "Left", "Right", "w", "b", "0",
            "$", "n", "N", "/", "ctrl+o", "Space", "ctrl+f", "L", "y",
        ];
        const VISION: &[&str] = &[
            "y", "Esc", "v", "h", "l", "Left", "Right", "w", "b", "0", "$", "q", "n", "N", "/",
            "ctrl+o", "i", "Enter", "t", "Space",
        ];
        let mut labels = SHARED.to_vec();
        labels.extend(match mode {
            Mode::Reading => READING,
            Mode::Cursor => CURSOR,
            Mode::Vision => VISION,
            _ => panic!("the table only drives the modes with text under a cursor"),
        });
        labels
            .into_iter()
            .map(|label| (label, press(label)))
            .collect()
    }

    /// What every press must do, one row per behaviour: keys that do the same
    /// thing share a row, and every key `presses` names has to land in one —
    /// a key missing from the table is a key nobody is watching. Only the
    /// landmark steps are counted exactly — a line, half a page, a page, a
    /// chapter — and every other row is named by what a reader would notice:
    /// where the cursor went, whether the page followed it, what was said.
    fn rows() -> Vec<(&'static [Mode], &'static [&'static str], Want)> {
        const ALL: &[Mode] = &[Mode::Reading, Mode::Cursor, Mode::Vision];
        const READING: &[Mode] = &[Mode::Reading];
        const CURSOR: &[Mode] = &[Mode::Cursor];
        const VISION: &[Mode] = &[Mode::Vision];
        const CURSOR_WORDS: &str = "cursor mode: Enter follows a link, i leaves";
        const VISION_WORDS: &str = "vision mode: move, y copies, Esc leaves";
        let next_match = || format!("{:?}  2/3", "cat");
        let no_more = || i18n::fill("no more matches for {}", &[&format!("{:?}", "cat")]);
        vec![
            // What all three modes answer alike.
            (ALL, &["?"], same().in_mode(Mode::Help)),
            // One line down and one line up, exactly.
            (ALL, &["j", "Down"], same().line(23).cleared()),
            (ALL, &["k", "Up"], same().line(19).cleared()),
            // Half a page in either direction, exactly.
            (ALL, &["ctrl+d"], same().line(31).cleared()),
            (ALL, &["ctrl+u"], same().line(11).cleared()),
            (ALL, &["g"], same()),
            (ALL, &["G"], same().forward()),
            (ALL, &["Q"], same().quits()),
            // The reading page: what it binds over the shared keys.
            (READING, &["q"], same().leaves()),
            (READING, &["Esc"], same().says(i18n::t("search cleared"))),
            // A page down and a page up, exactly, onto the chapter's last
            // and first screens.
            (READING, &["ctrl+f", "Space", "PageDown"], same().line(38).cleared()),
            (READING, &["ctrl+b", "Backspace", "PageUp"], same().line(5).cleared()),
            // A chapter hop: the next one, read from its top.
            (READING, &["L", "]", "Right"], same().line(0).in_chapter(1).cleared()),
            (READING, &["H", "[", "Left"], same().says(i18n::t("start of book"))),
            (READING, &["t", "Tab"], same().in_mode(Mode::Contents)),
            (READING, &["ctrl+o"], same().says(i18n::t("nowhere to go back to"))),
            (READING, &["i"], same().in_mode(Mode::Cursor).says(i18n::t(CURSOR_WORDS))),
            (READING, &["v"], same().in_mode(Mode::Vision).says(i18n::t(VISION_WORDS))),
            (READING, &["/"], same().in_mode(Mode::Search).cleared()),
            (READING, &["n"], same().line(13).says(next_match())),
            (READING, &["N"], same().says(no_more())),
            (READING, &["h", "l", "w", "b", "0", "$", "Enter", "y"], same()),
            // The cursor page.
            (CURSOR, &["Esc", "q", "i"], same().in_mode(Mode::Reading).cleared()),
            (CURSOR, &["v"], same().in_mode(Mode::Vision).says(i18n::t(VISION_WORDS))),
            (CURSOR, &["Enter"], same().line(0).in_chapter(1).cleared()),
            (CURSOR, &["t", "Tab"], same().in_mode(Mode::Contents)),
            (CURSOR, &["h", "Left", "b"], same().back().cleared()),
            (CURSOR, &["l", "Right", "w", "$"], same().forward().cleared()),
            (CURSOR, &["0"], same().cleared()),
            (CURSOR, &["n"], same().in_mode(Mode::Reading).line(13).says(next_match())),
            (CURSOR, &["N"], same().says(no_more())),
            (CURSOR, &["/"], same().in_mode(Mode::Search).cleared()),
            (CURSOR, &["ctrl+o"], same().says(i18n::t("nowhere to go back to"))),
            (CURSOR, &["Space", "ctrl+f", "L", "y"], same()),
            // The selection.
            (
                VISION,
                &["y"],
                same().in_mode(Mode::Cursor).says(i18n::fill("copied {} characters", &[&1])),
            ),
            (VISION, &["Esc", "v"], same().in_mode(Mode::Cursor).cleared()),
            (VISION, &["h", "Left", "b"], same().back().cleared()),
            (VISION, &["l", "Right", "w", "$"], same().forward().cleared()),
            (VISION, &["0"], same().cleared()),
            (VISION, &["q", "n", "N", "/", "ctrl+o", "i", "Enter", "t", "Space"], same()),
        ]
    }

    /// What a reader looked like before the press: where the cursor stood,
    /// which chapter, and the status standing.
    fn before_the_press(app: &App) -> ((usize, usize), usize, Option<String>) {
        (
            (app.cursor.line, app.cursor.column),
            app.chapter_index,
            app.status().map(str::to_string),
        )
    }

    /// One press's whole row, checked: the mode, the cursor, the page read
    /// around it, the status, the chapter and the way out.
    fn check(
        app: &App,
        mode: Mode,
        before: &((usize, usize), usize, Option<String>),
        want: &Want,
        at: &str,
    ) {
        assert_eq!(
            app.mode,
            want.mode.unwrap_or(mode),
            "{at}: the mode it ends in"
        );
        match want.cursor {
            Where::Stayed => assert_eq!(
                (app.cursor.line, app.cursor.column),
                before.0,
                "{at}: the cursor stayed"
            ),
            Where::Line(line) => assert_eq!(
                app.cursor.line, line,
                "{at}: the line the cursor stands on"
            ),
            Where::Forward => assert!(
                (app.cursor.line, app.cursor.column) > before.0,
                "{at}: the cursor went on"
            ),
            Where::Back => assert!(
                (app.cursor.line, app.cursor.column) < before.0,
                "{at}: the cursor went back"
            ),
        }
        // Half the view above the cursor, stopped at either end: the page is
        // read around wherever the press left the cursor.
        let centred = app
            .cursor
            .line
            .saturating_sub(app.view_height as usize / 2)
            .min(app.max_scroll());
        assert_eq!(app.scroll, centred, "{at}: the page follows the cursor");
        match &want.said {
            Said::Kept => assert_eq!(app.status(), before.2.as_deref(), "{at}: the status stayed"),
            Said::Cleared => assert!(app.status().is_none(), "{at}: the press said {:?}", app.status()),
            Said::Says(words) => {
                assert_eq!(app.status(), Some(words.as_str()), "{at}: what the press said")
            }
        }
        match want.chapter {
            Some(chapter) => assert_eq!(app.chapter_index, chapter, "{at}: the chapter it ends in"),
            None => assert_eq!(app.chapter_index, before.1, "{at}: the chapter stayed"),
        }
        assert_eq!(app.should_leave_book, want.out != Out::Stay, "{at}: leaving");
        assert_eq!(
            app.should_quit_program,
            want.out == Out::QuitProgram,
            "{at}: quitting"
        );
    }

    #[test]
    fn holding_j_hurries_the_reading_position_along() {
        // A chapter with room to run: the speed a hold builds up has to be
        // spendable before it reaches the end of the text.
        let body: String = (0..40)
            .map(|i| {
                format!(
                    "<p>Paragraph {i} runs long enough to wrap over several lines of a narrow column.</p>"
                )
            })
            .collect();
        let path = crate::testkit::TempBook::new("app-rush", "Rush", &[&body]);
        let (_dir, mut app) = crate::testapp::opened_default("app-rush-journal", &path);
        app.prepare(40, 14);
        let start = Instant::now();
        let before = app.cursor.line;
        // Twelve presses, fast enough to hurry each other the way a held key
        // does: the reading position ends further on than one line a press.
        for _ in 0..12 {
            app.handle_key(key(KeyCode::Char('j')));
        }
        let hurried = app.cursor.line - before;
        assert!(hurried > 12, "12 rapid presses moved {hurried} lines");
        // The pause the rush measures, handed in rather than slept out, so
        // the test keeps its milliseconds: one press is one line again.
        let paused = start + Duration::from_millis(600);
        let before = app.cursor.line;
        app.rush_line(1, paused);
        assert_eq!(
            app.cursor.line,
            before + 1,
            "after a pause, one press is one line"
        );
    }

    #[test]
    fn every_key_leaves_the_page_where_it_belongs() {
        // The three handlers used to repeat each other's keys; each row is
        // what the key does now the handlers are one, pressed from the state
        // a reader is really in: a search standing, the cursor on a link. A
        // row that changes means a key moved, woke up or went to sleep.
        let path = key_book("app-keys");
        let rows = rows();
        let mut pinned = 0;
        let mut missing = 0;
        for mode in [Mode::Reading, Mode::Cursor, Mode::Vision] {
            for (label, pressed) in presses(mode) {
                let Some((_, _, want)) = rows
                    .iter()
                    .find(|(modes, keys, _)| modes.contains(&mode) && keys.contains(&label))
                else {
                    missing += 1;
                    println!("({mode:?}, {label:?}),");
                    continue;
                };
                pinned += 1;
                let (_dir, mut app) = crate::testapp::opened_default("app-keys-journal", &path);
                keyed_app(&mut app, mode);
                // A row pins one press to one line, so the rush starts over
                // after whatever the setup did: this loop sends its presses
                // faster than any keyboard repeats, and would hurry the row
                // away from the line it was written to pin.
                app.rush.reset();
                let before = before_the_press(&app);
                app.handle_key(pressed);
                app.prepare(40, 14);
                check(&app, mode, &before, want, &format!("{mode:?}:{label}"));
            }
        }
        assert_eq!(missing, 0, "{missing} presses missing from the table");
        let named: usize = rows
            .iter()
            .map(|(modes, keys, _)| modes.len() * keys.len())
            .sum();
        assert_eq!(pinned, named, "a row waits on a key nobody presses");
    }

    #[test]
    fn stepping_over_a_chapter_boundary_lands_where_it_always_landed() {
        // `n` past the last match of a chapter opens the next one, and the
        // status and the match it picks must be what they always were — the
        // chapter is looked through on the way in, once, and the reader lands
        // on its first (or, going back, the old chapter's last) match.
        let path = key_book("app-keys-cross");
        let (_dir, mut app) =
            crate::testapp::opened_default("app-keys-cross-journal", &path);
        keyed_app(&mut app, Mode::Reading);
        // To the last match of the chapter the way a reader gets there, then
        // over the boundary.
        for _ in 0..3 {
            app.handle_key(key(KeyCode::Char('n')));
        }
        app.prepare(40, 14);
        assert_eq!(app.chapter_index, 1, "the next chapter was opened");
        assert_eq!(app.mode, Mode::Reading, "a match reads, it does not cursor");
        assert_eq!(
            app.cursor_text_position(),
            Some((0, 2)),
            "the first match of the new chapter is the one under the eye"
        );
        assert_eq!(app.scroll, 0, "and the page is read around it");
        assert_eq!(
            app.status(),
            Some(format!("{:?}  1/2", "cat").as_str()),
            "which match of how many, as ever"
        );

        // And back the same way: the old chapter's last match.
        app.handle_key(key(KeyCode::Char('N')));
        app.prepare(40, 14);
        assert_eq!(app.chapter_index, 0, "the previous chapter was opened");
        assert_eq!(app.mode, Mode::Reading);
        assert_eq!(
            app.cursor_text_position(),
            Some((6, 4)),
            "the last match of the chapter left behind"
        );
        assert_eq!(app.scroll, 16);
        assert_eq!(
            app.status(),
            Some(format!("{:?}  3/3", "cat").as_str()),
            "the last of three, as ever"
        );
    }
}
