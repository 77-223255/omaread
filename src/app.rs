//! Reader state and key handling.

use crate::doc::{Chapter, Locator};
use crate::epub::Book;
use crate::i18n;
use crate::identity::BookId;
use crate::journal::{BookRecord, Journal, State};
use crate::layout::{self, Index, LayoutOptions, Line};
use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Scrolling and reading. `j` and `k` move the page.
    Reading,
    /// A cursor sits in the text. `j` and `k` move the cursor.
    Normal,
    Contents,
    /// The key bindings.
    Help,
    /// Typing a search term.
    Search,
}

/// A position in the laid-out text: a line and a character within it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cursor {
    line: usize,
    column: usize,
}

/// A place to come back to.
///
/// Both parts matter: the view has to look as it did, and the cursor has to sit
/// on the link that was followed, not merely somewhere on its line.
#[derive(Debug, Clone)]
struct Jump {
    /// Topmost visible line, so the page looks the same.
    view: Locator,
    /// Where the cursor stood, in text coordinates.
    cursor: Option<(usize, usize)>,
}

/// One picture of the chapter: where it sits, where its bytes are, how much
/// room it takes, and why it is sized that way. Known before anything is
/// decoded.
struct ImageSlot {
    block: usize,
    src: String,
    rows: u16,
    /// Cells across, as `image::measure` counted them for this room. The view
    /// needs no more than the height, but the layout indents the picture to
    /// the centre of the column from this.
    cols: u16,
    reason: crate::image::Reason,
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
    cursor: Option<Cursor>,
    pending: Option<char>,
    status: Option<String>,
    pub should_quit: bool,
    /// True when the reader was left for good rather than to pick another book.
    pub should_quit_program: bool,
    options: LayoutOptions,
    id: BookId,
    journal: Journal,
    pending_restore: Option<Locator>,
    /// Pictures on screen, by block. Only what the view shows is kept: a
    /// chapter of rendered formulas holds hundreds of pictures, and decoding
    /// them all took twenty seconds before the first line appeared.
    images: std::collections::HashMap<usize, crate::image::Rendered>,
    /// Every picture of the chapter with the size it will take, measured from
    /// the image headers — the first 64 KB of each entry, nothing more. The
    /// layout needs all of them; decoding does not.
    image_slots: Vec<ImageSlot>,
    /// Pictures of this chapter whose bytes turned out to be undecodable: a
    /// header that measures is not a picture that draws. Their blocks must
    /// reserve nothing, and this chapter does not ask about them again —
    /// once a header has lied, re-reading it on every resize would only
    /// reserve the blank rows again.
    unreadable: std::collections::HashSet<usize>,
    /// Width and maximum height the slots were measured for. Rendering reuses
    /// it, so a picture comes out exactly as tall as the layout reserved.
    image_box: (u16, u16),
    /// How this terminal draws pictures, decided once at startup.
    image_backend: crate::image::Backend,
    /// Pixel size of one cell, needed to scale pictures to whole cells.
    cell_size: crate::image::CellSize,
    /// Colours from the active Omarchy theme.
    theme: crate::theme::Watcher,
    search: crate::search::Search,
    /// Where the reader was before following links, most recent last. `Ctrl-o`
    /// walks back out, as Vim's jump list does.
    jumps: Vec<Jump>,
    /// Cursor position to restore once the layout exists. A cursor lives on a
    /// line, which only exists after wrapping.
    pending_cursor: Option<(usize, usize)>,
    /// What has been typed into the search prompt so far.
    search_input: String,
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
        options: LayoutOptions,
    ) -> Result<Self> {
        let restore = state.position(&id).cloned();
        let title = name_shown(state.book(&id), book.title());

        // Open the chapter the position points into, not always the first.
        let chapter_index = restore
            .as_ref()
            .and_then(|locator| book.spine.iter().position(|item| item.href == locator.href))
            .unwrap_or(0);
        let chapter = load_chapter(&mut book, chapter_index);

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
            cursor: None,
            pending: None,
            status: None,
            should_quit: false,
            should_quit_program: false,
            options,
            id,
            journal,
            pending_restore: restore,
            images: std::collections::HashMap::new(),
            image_slots: Vec::new(),
            unreadable: std::collections::HashSet::new(),
            image_box: (0, 0),
            image_backend: crate::image::Backend::HalfBlocks,
            cell_size: crate::image::CellSize::default(),
            theme: crate::theme::Watcher::new(),
            search: crate::search::Search::default(),
            jumps: Vec::new(),
            pending_cursor: None,
            search_input: String::new(),
        })
    }

    /// Tells the reader how this terminal draws pictures. Set once at startup,
    /// after the terminal has been asked.
    pub fn set_image_backend(
        &mut self,
        backend: crate::image::Backend,
        cell_size: crate::image::CellSize,
    ) {
        self.image_backend = backend;
        self.cell_size = cell_size;
        self.images.clear();
        self.invalidate_layout();
    }

    pub fn search(&self) -> &crate::search::Search {
        &self.search
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
        self.theme.theme()
    }

    /// Re-reads the theme file when it changed, which happens on every Omarchy
    /// theme switch. Returns true when the colours moved, so the caller can
    /// repaint.
    pub fn refresh_theme(&mut self) -> bool {
        self.theme.refresh()
    }

    pub fn image_backend(&self) -> crate::image::Backend {
        self.image_backend
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
        self.image_backend != crate::image::Backend::HalfBlocks && !self.images.is_empty()
    }

    // ----- reporting for the view -----

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn chapter_title(&self) -> String {
        self.book
            .spine
            .get(self.chapter_index)
            .and_then(|item| item.title.clone())
            .unwrap_or_else(|| format!("Chapter {}", self.chapter_index + 1))
    }

    pub fn chapter_number(&self) -> (usize, usize) {
        (self.chapter_index + 1, self.book.spine.len())
    }

    pub fn contents(&self) -> Vec<String> {
        self.book
            .spine
            .iter()
            .enumerate()
            .map(|(i, item)| {
                item.title
                    .clone()
                    .unwrap_or_else(|| format!("Chapter {}", i + 1))
            })
            .collect()
    }

    pub fn contents_cursor(&self) -> usize {
        self.contents_cursor
    }

    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    /// Works out how much room each of this chapter's pictures takes.
    ///
    /// Only the first 64 KB of each entry is read — the header, and nothing
    /// after it — so this stays cheap however many pictures a chapter holds
    /// and however often a window resize rebuilds it. A picture whose header
    /// cannot be read, or whose block already failed to decode, gets no slot,
    /// and the layout falls back to its alt text. A broken image must not cost
    /// the chapter.
    fn measure_images(&mut self, width: u16) {
        self.images.clear();
        self.image_slots.clear();
        self.image_box = (0, 0);
        if width < 8 {
            return;
        }
        // A picture belongs in the same column as the text, so it is measured
        // against the same width: `layout_full` narrows the window this way
        // too. Given the whole window instead, a picture broke out of the text
        // on both counts, too wide and, scaled in proportion, too tall.
        let width = width.min(self.options.max_width).max(8);
        // The room is the whole text area: the viewport with its one status
        // row taken off, which is what `view_height` already counts. The
        // status line is the only row a picture never gets, so a picture may
        // reach the bottom of the view — no four-fifths share, no floor.
        let max_rows = self.view_height;
        self.image_box = (width, max_rows);

        let sources: Vec<(usize, String)> = self
            .chapter
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(index, block)| match &block.kind {
                crate::doc::BlockKind::Image { src: Some(src) } => Some((index, src.clone())),
                _ => None,
            })
            .collect();

        for (index, src) in sources {
            // A picture that already failed to decode reserves nothing and is
            // not tried again for this chapter.
            if self.unreadable.contains(&index) {
                continue;
            }
            // What the picture is in the book decides the box it is measured
            // for, long before its bytes are read.
            let reason = self.book.picture_reason(&src);
            let Ok(bytes) = self.book.read_header(&src) else {
                continue;
            };
            let Ok(size) = crate::image::dimensions(&bytes) else {
                continue;
            };
            let (cols, rows) = crate::image::measure(
                size,
                width,
                max_rows,
                reason.role(),
                self.image_backend,
                self.cell_size,
            );
            if rows == 0 {
                continue;
            }
            self.image_slots.push(ImageSlot {
                block: index,
                src,
                rows,
                cols,
                reason,
            });
        }
    }

    /// How much room the layout must leave for each picture.
    fn image_placements(&self) -> Vec<layout::ImagePlacement> {
        self.image_slots
            .iter()
            .map(|slot| layout::ImagePlacement {
                block: slot.block,
                rows: slot.rows,
                cols: slot.cols,
            })
            .collect()
    }

    /// Decodes the pictures the view shows and drops the rest.
    ///
    /// Called before every draw, because scrolling changes which ones are
    /// needed. Decoding one picture costs milliseconds; decoding a chapter of
    /// them costs seconds, which is why only what is on screen is done.
    fn render_visible(&mut self) {
        let (width, max_rows) = self.image_box;
        if width == 0 || self.image_slots.is_empty() {
            return;
        }
        let height = self.view_height as usize;
        let blocks_between = |from: usize, to: usize| -> std::collections::HashSet<usize> {
            let from = from.min(self.lines.len());
            let to = to.min(self.lines.len());
            self.lines[from..to]
                .iter()
                .filter(|line| matches!(line.kind, layout::LineKind::Image { .. }))
                .map(|line| line.block)
                .collect()
        };
        let visible = blocks_between(self.scroll, self.scroll + height);
        // Reading moves back as well as forward, and a picture just off the edge
        // is about to be wanted again. Keeping a screen either way spares the
        // decoding, and a handful of pictures is nothing to hold.
        let nearby = blocks_between(self.scroll.saturating_sub(height), self.scroll + height * 2);

        self.images.retain(|block, _| nearby.contains(block));

        // The slot's position is its Kitty id, so a picture keeps the same id
        // however often it leaves the screen and comes back.
        let pending: Vec<(u32, usize, String, crate::image::Reason)> = self
            .image_slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| {
                visible.contains(&slot.block) && !self.images.contains_key(&slot.block)
            })
            // Ids start at 1; 0 is reserved by the Kitty protocol.
            .map(|(id, slot)| (id as u32 + 1, slot.block, slot.src.clone(), slot.reason))
            .collect();

        // Whether a picture turned out to be undecodable this time round. A
        // block must not hold rows for a picture that draws nothing, so the
        // layout is rebuilt once, without the ones that failed.
        let mut broken = false;
        for (id, block, src, reason) in pending {
            // Drawing needs the whole picture, where measuring only needed
            // the header.
            let rendered = self.book.read_binary(&src).and_then(|bytes| {
                crate::image::render(
                    &bytes,
                    width,
                    max_rows,
                    reason.role(),
                    self.image_backend,
                    id,
                    self.cell_size,
                )
            });
            match rendered {
                Ok(rendered) if rendered.height() > 0 => {
                    self.images.insert(block, rendered);
                }
                // The header measured but the bytes do not decode — a lying
                // or truncated header. The block stops reserving rows for a
                // picture that will never appear, and this chapter does not
                // ask about these bytes again.
                _ => {
                    self.images.remove(&block);
                    self.unreadable.insert(block);
                    broken = true;
                }
            }
        }
        if broken {
            // One rebuild, without the pictures that failed: the next frame's
            // `prepare` runs the layout again and `measure_images` skips them.
            self.invalidate_layout();
        }
    }

    /// The picture of a block, once rendered.
    pub fn image_at(&self, block: usize) -> Option<&crate::image::Rendered> {
        self.images.get(&block)
    }

    /// The link the cursor sits on, if any.
    pub fn link_at_cursor(&self) -> Option<&crate::doc::Link> {
        let (block, offset) = self.cursor_text_position()?;
        self.chapter.link_at(block, offset)
    }

    /// The cursor's screen position, when a cursor exists.
    pub fn cursor_position(&self) -> Option<(usize, usize)> {
        self.cursor.map(|c| (c.line, c.column))
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
    /// position. Called before each draw. `height` is the whole viewport: the
    /// status row comes off here, so the text area the layout builds and the
    /// room pictures are measured for are the same rows the view draws in.
    pub fn prepare(&mut self, width: u16, height: u16) {
        self.view_height = text_rows(height).max(1);
        if width == self.laid_out_for && !self.lines.is_empty() {
            self.clamp_scroll();
            // Scrolling moved other pictures into view, even though the layout
            // still holds.
            self.render_visible();
            return;
        }
        // Remember text positions, not line numbers: the wrap is about to change.
        let anchor_text = self.pending_restore.take().or_else(|| self.position());
        let cursor_text = self.cursor_text_position();

        self.measure_images(width);
        let placements = self.image_placements();
        self.lines = layout::layout_full(&self.chapter, width, self.options, &placements);
        self.laid_out_for = width;

        if let Some(anchor) = anchor_text {
            self.scroll = self.line_for(&anchor);
        }
        // A cursor being restored outranks the one that was there: it exists only
        // until the first layout has been built.
        if let Some((block, offset)) = self.pending_cursor.take().or(cursor_text) {
            self.cursor = self.cursor_at(block, offset);
        }
        self.clamp_scroll();
        // Last, because it needs the finished lines and the settled scroll.
        self.render_visible();
    }

    /// Marks the layout as stale, so the next draw builds it again. Needed when
    /// something the layout was built from has changed, such as how much room a
    /// picture takes.
    fn invalidate_layout(&mut self) {
        self.laid_out_for = 0;
    }

    pub fn position(&self) -> Option<Locator> {
        let line = self.lines.get(self.scroll)?;
        Some(Locator {
            href: self.chapter.href.clone(),
            block: line.block,
            offset: line.offset,
        })
    }

    fn line_for(&self, locator: &Locator) -> usize {
        let mut best = 0;
        for (index, line) in self.lines.iter().enumerate() {
            if line.block < locator.block
                || (line.block == locator.block && line.offset <= locator.offset)
            {
                best = index;
            } else {
                break;
            }
        }
        best
    }

    /// The cursor as a block and character offset.
    fn cursor_text_position(&self) -> Option<(usize, usize)> {
        let cursor = self.cursor?;
        let line = self.lines.get(cursor.line)?;
        Some((line.block, line.offset + cursor.column))
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

    fn scroll_by(&mut self, delta: isize) {
        let target = self.scroll as isize + delta;
        self.scroll = target.clamp(0, self.max_scroll() as isize) as usize;
        self.status = None;
    }

    /// Scrolls just enough to keep the cursor on screen.
    fn follow_cursor(&mut self) {
        let Some(cursor) = self.cursor else { return };
        let height = self.view_height as usize;
        if cursor.line < self.scroll {
            self.scroll = cursor.line;
        } else if cursor.line >= self.scroll + height {
            self.scroll = cursor.line + 1 - height;
        }
        self.clamp_scroll();
    }

    // ----- keys -----

    pub fn handle_key(&mut self, key: KeyEvent) {
        if let Some(first) = self.pending.take() {
            if self.handle_sequence(first, key) {
                return;
            }
        }
        match self.mode {
            Mode::Reading => self.handle_reading_key(key),
            Mode::Normal => self.handle_cursor_key(key),
            Mode::Contents => self.handle_contents_key(key),
            Mode::Search => self.handle_search_key(key),
            // Any key closes the help; there is nothing to do in it.
            Mode::Help => {
                self.mode = Mode::Reading;
                self.status = None;
            }
        }
    }

    fn handle_sequence(&mut self, first: char, key: KeyEvent) -> bool {
        match (first, key.code) {
            ('g', KeyCode::Char('g')) => {
                match self.mode {
                    Mode::Contents => self.contents_cursor = 0,
                    Mode::Normal => {
                        self.cursor = self.first_cursor();
                        self.follow_cursor();
                    }
                    Mode::Reading => self.scroll = 0,
                    Mode::Help | Mode::Search => {}
                }
                true
            }
            _ => false,
        }
    }

    fn handle_reading_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if key.code == KeyCode::Char('?') {
            self.mode = Mode::Help;
            return;
        }
        let half = (self.view_height / 2).max(1) as isize;
        let page = (self.view_height.saturating_sub(2)).max(1) as isize;

        match key.code {
            // Only `q` quits. Esc takes back what is in force, which is what a
            // Vim user expects, and it must never end the session by accident.
            // `q` leaves the book. Coming from the library that means going
            // back to it; started with a file it means leaving.
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('Q') => {
                self.should_quit = true;
                self.should_quit_program = true;
            }
            KeyCode::Esc => self.dismiss(),
            KeyCode::Char('j') | KeyCode::Down => self.scroll_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll_by(-1),
            KeyCode::Char('d') if ctrl => self.scroll_by(half),
            KeyCode::Char('u') if ctrl => self.scroll_by(-half),
            KeyCode::Char('f') if ctrl => self.scroll_by(page),
            KeyCode::Char('b') if ctrl => self.scroll_by(-page),
            KeyCode::Char(' ') | KeyCode::PageDown => self.scroll_by(page),
            KeyCode::Backspace | KeyCode::PageUp => self.scroll_by(-page),
            KeyCode::Char('g') => self.pending = Some('g'),
            KeyCode::Char('G') => self.scroll = self.max_scroll(),
            KeyCode::Char('L') | KeyCode::Char(']') => self.next_chapter(),
            KeyCode::Char('H') | KeyCode::Char('[') => self.previous_chapter(),
            KeyCode::Char('t') | KeyCode::Tab => self.open_contents(),
            KeyCode::Char('o') if ctrl => self.jump_back(),
            // Entering normal mode puts a cursor into the text.
            KeyCode::Char('i') => self.enter_normal_mode(),
            KeyCode::Char('/') => self.open_search(),
            KeyCode::Char('n') => self.jump_to_match(true),
            KeyCode::Char('N') => self.jump_to_match(false),
            _ => {}
        }
    }

    fn handle_cursor_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if key.code == KeyCode::Char('?') {
            self.mode = Mode::Help;
            return;
        }
        let half = (self.view_height / 2).max(1) as isize;

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.leave_normal_mode(),
            KeyCode::Char('Q') => {
                self.should_quit = true;
                self.should_quit_program = true;
            }
            KeyCode::Char('i') => self.leave_normal_mode(),
            KeyCode::Char('/') => self.open_search(),
            KeyCode::Char('n') => self.jump_to_match(true),
            KeyCode::Char('N') => self.jump_to_match(false),
            KeyCode::Char('h') | KeyCode::Left => self.move_cursor_horizontally(-1),
            KeyCode::Char('l') | KeyCode::Right => self.move_cursor_horizontally(1),
            KeyCode::Char('j') | KeyCode::Down => self.move_cursor_vertically(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_cursor_vertically(-1),
            KeyCode::Char('d') if ctrl => self.move_cursor_vertically(half),
            KeyCode::Char('u') if ctrl => self.move_cursor_vertically(-half),
            KeyCode::Char('w') => self.move_cursor_word(true),
            KeyCode::Char('b') => self.move_cursor_word(false),
            KeyCode::Char('0') => self.move_cursor_to_line_edge(false),
            KeyCode::Char('$') => self.move_cursor_to_line_edge(true),
            KeyCode::Char('g') => self.pending = Some('g'),
            KeyCode::Char('G') => {
                self.cursor = self.last_cursor();
                self.follow_cursor();
            }
            KeyCode::Char('t') | KeyCode::Tab => self.open_contents(),
            // Enter follows the link under the cursor; Ctrl-o walks back out,
            // as it does in Vim.
            KeyCode::Enter => self.follow_link(),
            KeyCode::Char('o') if ctrl => self.jump_back(),
            _ => {}
        }
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
                self.search.scan(&self.chapter);
                // Start from where the reader stands, not from the chapter's top.
                let from = self
                    .position()
                    .map(|p| (p.block, p.offset))
                    .unwrap_or((0, 0));
                if self.search.go_to_first_after(from.0, from.1) {
                    self.show_current_match();
                } else {
                    // Nothing further on in this chapter, so look on ahead.
                    self.jump_to_match(true);
                }
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
            KeyCode::Char('g') => self.pending = Some('g'),
            KeyCode::Char('G') => self.contents_cursor = last,
            KeyCode::Enter => {
                let target = self.contents_cursor;
                self.go_to_chapter(target);
                self.mode = Mode::Reading;
            }
            _ => {}
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
        self.mode = Mode::Normal;
        if self.cursor.is_none() || !self.cursor_is_visible() {
            // Start at the top of the view, where the eye already is.
            self.cursor = Index::new(&self.lines)
                .next_selectable(self.scroll)
                .map(|line| Cursor { line, column: 0 });
        }
        self.status = Some(i18n::t("cursor mode: Enter follows a link, i leaves").into());
    }

    fn cursor_is_visible(&self) -> bool {
        match self.cursor {
            Some(cursor) => {
                cursor.line >= self.scroll && cursor.line < self.scroll + self.view_height as usize
            }
            None => false,
        }
    }

    fn leave_normal_mode(&mut self) {
        self.mode = Mode::Reading;
        self.status = None;
    }

    fn move_cursor_horizontally(&mut self, delta: isize) {
        let Some(mut cursor) = self.cursor else {
            return;
        };
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
        } else if cursor.line > 0 {
            if let Some(previous) = index.previous_selectable(cursor.line - 1) {
                cursor = Cursor {
                    line: previous,
                    column: self.lines[previous].text_len().saturating_sub(1),
                };
            }
        }
        self.cursor = Some(cursor);
        self.status = None;
        self.follow_cursor();
    }

    fn move_cursor_vertically(&mut self, delta: isize) {
        let Some(cursor) = self.cursor else { return };
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
        self.cursor = Some(Cursor { line, column });
        self.status = None;
        self.follow_cursor();
    }

    fn move_cursor_to_line_edge(&mut self, end: bool) {
        let Some(mut cursor) = self.cursor else {
            return;
        };
        cursor.column = if end {
            self.lines[cursor.line].text_len().saturating_sub(1)
        } else {
            0
        };
        self.cursor = Some(cursor);
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
                self.cursor = self.cursor_at(block, offset);
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
        self.search.scan(&self.chapter);
        if self.search.go_to_first() {
            self.show_current_match();
        } else {
            // Not in this chapter: look on through the book.
            self.jump_to_match(true);
        }
    }

    pub fn save_position(&mut self) {
        let Some(locator) = self.position() else {
            return;
        };
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
        let here = Jump {
            view: self.position().unwrap_or_else(|| Locator {
                href: self.chapter.href.clone(),
                block: 0,
                offset: 0,
            }),
            // The cursor is on the link being followed, which is exactly where
            // coming back should put it.
            cursor: self.cursor_text_position(),
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
        self.pending_cursor = Some((landing.0, landing.1));
        self.mode = Mode::Normal;
        self.pending_restore = Some(Locator {
            href: self.chapter.href.clone(),
            block: landing.0,
            offset: landing.1,
        });
        self.laid_out_for = 0;
        self.lines.clear();
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
        if let Some(index) = self.find_in_spine(&back.view.href) {
            if index != self.chapter_index {
                // Going back must not push another entry onto the stack.
                self.chapter_index = index;
                self.chapter = load_chapter(&mut self.book, index);
                self.images.clear();
                // This chapter's pictures have not been tried yet.
                self.unreadable.clear();
                self.contents_cursor = index;
                self.cursor = None;
                if self.search.is_active() {
                    self.search.scan(&self.chapter);
                }
            }
        }
        self.pending_restore = Some(back.view);
        self.pending_cursor = back.cursor;
        // Coming back into the text means the cursor is wanted, so the mode
        // follows it rather than dropping to plain reading.
        if back.cursor.is_some() && self.mode == Mode::Reading {
            self.mode = Mode::Normal;
        }
        self.laid_out_for = 0;
        self.lines.clear();
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

            let chapter = load_chapter(&mut self.book, index);
            let hits = crate::search::find_all(&chapter, self.search.query());
            if hits.is_empty() {
                continue;
            }
            // Found one: move there and pick the first or last match.
            self.save_position();
            self.chapter_index = index;
            self.chapter = chapter;
            self.contents_cursor = index;
            self.images.clear();
            // This chapter's pictures have not been tried yet.
            self.unreadable.clear();
            self.cursor = None;
            self.search.scan(&self.chapter);
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
        let Some((block, offset)) = self.search.current_position() else {
            return;
        };
        self.pending_restore = Some(Locator {
            href: self.chapter.href.clone(),
            block,
            offset,
        });
        // Force a rebuild so the pending position is applied.
        self.laid_out_for = 0;
        self.lines.clear();
        self.mode = Mode::Reading;
        self.status = match self.search.progress() {
            Some((at, total)) => Some(format!("{:?}  {at}/{total}", self.search.query())),
            None => None,
        };
    }

    fn open_contents(&mut self) {
        self.contents_cursor = self.chapter_index;
        self.mode = Mode::Contents;
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
        self.chapter_index = index;
        self.chapter = load_chapter(&mut self.book, index);
        self.images.clear();
        // This chapter's pictures have not been tried yet.
        self.unreadable.clear();
        if self.search.is_active() {
            self.search.scan(&self.chapter);
        }
        self.contents_cursor = index;
        self.scroll = 0;
        self.cursor = None;
        if self.mode == Mode::Normal {
            self.mode = Mode::Reading;
        }
        // Force a rebuild on the next draw.
        self.laid_out_for = 0;
        self.lines.clear();
        self.status = None;
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
                ("j k", t("line down, up")),
                ("Space Backspace", t("page down, up")),
                ("Ctrl-d Ctrl-u", t("half a page")),
                ("gg G", t("start, end of chapter")),
                ("L ]", t("next chapter")),
                ("H [", t("previous chapter")),
                ("t Tab", t("contents")),
                ("/", t("search the book")),
                ("n N", t("next, previous match")),
                ("i", t("cursor mode, with a cursor in the text")),
                ("Esc", t("clear the search")),
                ("q", t("back to the library")),
                ("Q", t("quit")),
            ],
        ),
        (
            "Cursor",
            vec![
                ("h l w b 0 $", t("move by character, word, to line edge")),
                ("j k gg G", t("move by line, to chapter edges")),
                ("Enter", t("follow the link under the cursor")),
                ("Ctrl-o", t("back out of followed links")),
                ("/ n N", t("search, next match, previous match")),
                ("Esc i", t("leave the cursor")),
            ],
        ),
        (
            "Contents",
            vec![
                ("j k gg G", t("move the cursor")),
                ("Enter", t("open the chapter")),
                ("q Esc", t("close")),
            ],
        ),
    ]
}

/// Splits a link target into its file part and its fragment.
fn split_target(target: &str) -> (&str, Option<&str>) {
    match target.split_once('#') {
        Some((file, fragment)) if fragment.is_empty() => (file, None),
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

/// Loads a chapter, turning a parse failure into a readable placeholder rather
/// than ending the session.
fn load_chapter(book: &mut Book, index: usize) -> Chapter {
    match book.chapter(index) {
        Ok(chapter) => chapter,
        Err(err) => {
            let href = book
                .spine
                .get(index)
                .map(|i| i.href.clone())
                .unwrap_or_default();
            Chapter {
                href,
                links: Vec::new(),
                anchors: std::collections::HashMap::new(),
                blocks: vec![crate::doc::Block {
                    kind: crate::doc::BlockKind::Paragraph,
                    runs: vec![crate::doc::Run {
                        text: format!("This chapter could not be read: {err}"),
                        style: crate::doc::RunStyle::default(),
                    }],
                }],
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// A book whose one drawable picture has a header that measures and bytes
    /// that do not decode — a PNG cut just after its IDAT header — plus a picture
    /// that is not in the container at all.
    fn liar_book() -> (std::path::PathBuf, Book) {
        use std::io::Write;
        let solid = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            100,
            100,
            image::Rgba([10, 20, 30, 255]),
        ));
        let mut encoded = std::io::Cursor::new(Vec::new());
        solid
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        let png = encoded.into_inner();
        // Cut just past the IDAT chunk's header: the size still reads from
        // the IHDR, and the pixel stream is gone, so nothing can be drawn.
        let idat = png
            .windows(4)
            .position(|window| window == b"IDAT")
            .expect("a PNG has an IDAT chunk");
        let liar = &png[..idat + 8];

        let path = std::env::temp_dir().join("omaread-app-liar.epub");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        let mut write = |name: &str, body: &[u8]| {
            zip.start_file(name, options).unwrap();
            zip.write_all(body).unwrap();
        };
        write(
            "META-INF/container.xml",
            br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
<rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#,
        );
        write(
            "OEBPS/content.opf",
            br#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Liar</dc:title></metadata>
<manifest>
  <item id="c0" href="ch0.xhtml" media-type="application/xhtml+xml"/>
  <item id="bad" href="bad.png" media-type="image/png"/>
  <item id="gone" href="gone.png" media-type="image/png"/>
</manifest>
<spine><itemref idref="c0"/></spine></package>"#,
        );
        write(
            "OEBPS/ch0.xhtml",
            br#"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>t</title></head>
<body><p>before</p><img src="bad.png" alt="liar"/><img src="gone.png" alt="missing"/><p>after</p></body></html>"#,
        );
        write("OEBPS/bad.png", liar);
        zip.finish().unwrap();
        let book = Book::open(&path).unwrap();
        (path, book)
    }

    #[test]
    fn a_picture_that_cannot_be_decoded_reserves_nothing() {
        // The header measures — 100x100 — but the pixel stream was cut, so
        // nothing can be drawn. The block must not go on holding rows for a
        // picture that draws nothing: the failed decode marks it unreadable,
        // the chapter is laid out once more without it, and those bytes are
        // never tried again for this chapter. The picture that is missing
        // from the container entirely never gets a slot in the first place.
        ///
        /// How many rows one block is given as a picture. A picture that
        /// measured gets as many as it is tall; without pixels the alt text
        /// has to do, and that is one row.
        fn picture_rows(app: &App, block: usize) -> usize {
            app.lines
                .iter()
                .filter(|line| {
                    line.block == block && matches!(line.kind, layout::LineKind::Image { .. })
                })
                .count()
        }

        let (path, book) = liar_book();
        let dir = std::env::temp_dir().join("omaread-app-liar-journal");
        std::fs::remove_dir_all(&dir).ok();
        let journal = Journal::open(&dir).unwrap();
        let state = State::default();
        let id = BookId::from(format!("sha256:{}", "01".repeat(32)));
        let mut app = App::new(
            book,
            id,
            journal,
            &state,
            LayoutOptions {
                max_width: u16::MAX,
            },
        )
        .unwrap();

        // First frame: the header is believed and the block gets its rows —
        // only for as long as it takes to find out that the bytes are bad.
        app.prepare(80, 24);
        assert_eq!(
            app.image_slots.len(),
            1,
            "the lying header measured; the missing picture never does"
        );
        let block = app.image_slots[0].block;
        assert!(
            picture_rows(&app, block) > 1,
            "the layout believed the header and reserved the picture's rows"
        );
        assert!(app.unreadable.contains(&block), "the decode failed");

        // The rebuilt layout: the picture's rows are gone — only the one row
        // of alt text is left — and there is no second attempt.
        app.prepare(80, 24);
        assert!(app.image_slots.is_empty(), "the block reserves nothing");
        assert_eq!(
            picture_rows(&app, block),
            1,
            "the rows went back to the text, leaving the alt text"
        );
        app.prepare(80, 24);
        assert!(
            app.image_slots.is_empty(),
            "the chapter does not try those bytes again"
        );
        std::fs::remove_file(path).ok();
        std::fs::remove_dir_all(&dir).ok();
    }
}
