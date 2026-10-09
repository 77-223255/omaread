//! Drawing the reader.
//!
//! The lines come from `layout` already wrapped and set, so this module only
//! turns them into cells, painting the search, the cursor and the vision
//! mode's selection over whatever the line already carries.

mod hits;
mod shelf;

pub use hits::draw_hits;
pub use shelf::draw_shelf;

use crate::app::{App, Mode};
use crate::i18n;
use crate::layout::{Line as SourceLine, LineKind};
use crate::measure::{cells, fit};
use crate::theme::text_color_on;
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

/// Columns kept free left and right of the text.
const SIDE_MARGIN: u16 = 2;

/// The narrowest frame that keeps its margins. Below this the two cells of air
/// on each side go back to the page: on a small screen a cell of text, or a
/// cell of a cover, is worth more than a cell of margin.
const MIN_WIDTH_FOR_SIDE_MARGIN: u16 = 20;

/// The smallest frame that still shows a status row. Below either number the
/// row is dropped and the page takes the whole frame — a line about how many
/// books there are, or where you are in one, is worth less than the line of the
/// book or the row of a cover it would cost.
const STATUS_MIN_WIDTH: u16 = 30;
const STATUS_MIN_HEIGHT: u16 = 4;

/// The margin kept on a frame this wide.
fn margin(width: u16) -> u16 {
    if width >= MIN_WIDTH_FOR_SIDE_MARGIN {
        SIDE_MARGIN
    } else {
        0
    }
}

/// Whether the frame has room for both a page and the one status row.
fn status_shown(area: Rect) -> bool {
    area.width >= STATUS_MIN_WIDTH && area.height >= STATUS_MIN_HEIGHT
}

/// True when the terminal has shrunk to a single cell along either side: a
/// screen one cell thick has nothing left to show, and a face is all there is.
fn too_small(area: Rect) -> bool {
    area.width <= 1 || area.height <= 1
}

/// Draws the face omaread makes when the terminal is down to a single cell: one
/// line, in the middle, in the colour it keeps for last.
fn draw_too_small(frame: &mut Frame, area: Rect, theme: &crate::theme::Theme) {
    frame.render_widget(Clear, area);
    if area.width == 0 || area.height == 0 {
        return;
    }
    let row = Rect {
        y: area.y + area.height / 2,
        height: 1,
        ..area
    };
    // One colour, the theme's own emphasis: a face is not a reason to invent
    // a palette the rest of the reader does not share.
    let face = Style::default()
        .fg(rgb(theme.accent))
        .add_modifier(Modifier::BOLD);
    let line = Line::from(vec![
        Span::styled(":", face),
        Span::styled("(", face),
    ]);
    frame.render_widget(Paragraph::new(line).alignment(Alignment::Center), row);
}

fn rgb((r, g, b): crate::theme::Rgb) -> Color {
    Color::Rgb(r, g, b)
}

/// How much of the way from the page's own colour to the text's colour the
/// text outside the focus keeps. Low enough that the paragraph in focus
/// stands out at a glance, high enough to go on reading.
const FOCUS_FADE: f32 = 0.55;

/// How far the body text is pushed toward white.
///
/// A theme's foreground is picked for a whole desktop, and a page of it at
/// arm's length reads dimmer than the line of UI chrome beside it. Lifting the
/// colour keeps the hue and the theme's character while making the text itself
/// the brightest thing in the room.
const TEXT_BOOST: f32 = 0.18;

/// A text colour pushed `TEXT_BOOST` of the way toward white.
fn brightened((r, g, b): crate::theme::Rgb) -> Color {
    let lift = |channel: u8| {
        (channel as f32 + (255.0 - channel as f32) * TEXT_BOOST).round() as u8
    };
    Color::Rgb(lift(r), lift(g), lift(b))
}

/// Blends a style's colours toward what lies behind them: what the text
/// outside the focus wears. Blending the colour rather than adding the faint
/// attribute keeps the step back the same size in every terminal, and keeps
/// the hue so a heading still reads as a heading.
fn faded_style(style: Style, theme: &crate::theme::Theme) -> Style {
    let colour = match style.fg {
        Some(Color::Rgb(r, g, b)) => (r, g, b),
        _ => theme.foreground,
    };
    let behind = match style.bg {
        Some(Color::Rgb(r, g, b)) => (r, g, b),
        _ => theme.background,
    };
    let channel = |from: u8, to: u8| {
        (to as f32 + (from as f32 - to as f32) * FOCUS_FADE).round() as u8
    };
    style.fg(Color::Rgb(
        channel(colour.0, behind.0),
        channel(colour.1, behind.1),
        channel(colour.2, behind.2),
    ))
}

/// Where a picture goes on screen, for the backends that paint past the text
/// buffer. Collected during the draw and written afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// The block whose picture goes here, so the writer can find its bytes
    /// without the view copying them.
    pub block: usize,
    /// Column and row on screen, zero-based.
    pub column: u16,
    pub row: u16,
    /// Which rows of the picture the screen shows: its first visible row, and
    /// how many of its rows are visible from there. A picture taller than the
    /// text area — or one whose top has scrolled away — is drawn down to the
    /// row where the area ends, instead of not being drawn at all.
    pub crop: (u16, u16),
}

/// The frame split into the text area above and the one status row below.
///
/// The split is built from `app::text_rows` — the reader's own count of the
/// rows a picture may have — so the two cannot drift apart: one definition,
/// and a picture whose room is taller than the area it is drawn in cannot
/// run under the status line. On a small frame the row is dropped and the
/// text area is the whole frame; the reader is told the same height, so the
/// room for pictures and the rows they are drawn in are still one number.
pub fn split_frame(area: Rect) -> (Rect, Rect) {
    let text_height = if status_shown(area) {
        crate::app::text_rows(area.height)
    } else {
        area.height
    };
    let text = Rect {
        height: text_height,
        ..area
    };
    let status = Rect {
        y: area.y + text_height,
        height: area.height - text_height,
        ..area
    };
    (text, status)
}

/// The frame as every screen reads it: the text area, the text area with its
/// side margins taken off, and the status row — or nothing at all when the
/// terminal is down to a single cell, where the face draws instead.
///
/// One place for the split and the margin, so the page, the shelf and the hit
/// list cannot drift apart on where the margin falls or when the face
/// appears.
fn page_areas(
    frame: &mut Frame,
    theme: &crate::theme::Theme,
) -> Option<(Rect, Rect, Rect)> {
    if too_small(frame.area()) {
        draw_too_small(frame, frame.area(), theme);
        return None;
    }
    let (text, status) = split_frame(frame.area());
    let edge = margin(text.width);
    let inner = Rect {
        x: text.x + edge,
        y: text.y,
        width: text.width.saturating_sub(edge * 2),
        height: text.height,
    };
    Some((text, inner, status))
}

pub fn draw(frame: &mut Frame, app: &mut App) -> Vec<Placement> {
    let Some((text_area, inner, status_area)) = page_areas(frame, &app.theme()) else {
        return Vec::new();
    };

    // The text area, not the frame: the reader takes the status row off itself
    // only while there is one, so the room for pictures and the rows the layout
    // reserves for them are worked out in one place, from one height.
    app.prepare(inner.width, text_area.height);
    draw_text(frame, inner, app);
    if status_area.height > 0 {
        draw_status(frame, status_area, app);
    }

    match app.mode {
        Mode::Contents => {
            draw_contents(frame, text_area, app);
            // An overlay covers the text, so pictures must not be painted over it.
            return Vec::new();
        }
        Mode::Help => {
            draw_help(frame, text_area, app);
            return Vec::new();
        }
        _ => {}
    }
    placements(inner, app)
}

/// Collects the pictures visible in this frame, with the screen position each
/// one starts at and how much of it is on screen. One placement per picture:
/// the picture's own top row when the view still shows it, or the top of the
/// viewport when the picture runs off the top — the protocol paints downwards
/// from there, so the first row the reader can see is where it starts.
fn placements(area: Rect, app: &App) -> Vec<Placement> {
    let mut out = Vec::new();
    for (viewport_row, line) in app.visible_lines().iter().enumerate() {
        let LineKind::Image { row, rows } = line.kind else {
            continue;
        };
        // The first visible row of this picture, and no other: its top when
        // that is still in the view, otherwise the viewport's own first row.
        if row != 0 && viewport_row != 0 {
            continue;
        }
        let Some(rendered) = app.image_at(line.block) else {
            continue;
        };
        // Only pixels need placing: the block backend draws ordinary cells
        // that clip with the text around them.
        if !rendered.is_pixel() {
            continue;
        }
        // The picture runs from here to the bottom of the text area — the
        // terminal paints these pixels itself, so nothing would clip them at
        // the edge and a lower half would cover the status line.
        let visible = (rows - row).min(area.height.saturating_sub(viewport_row as u16));
        if visible == 0 {
            continue;
        }
        out.push(Placement {
            block: line.block,
            column: area.x + line.indent,
            row: area.y + viewport_row as u16,
            crop: (row, visible),
        });
    }
    out
}

/// What a single character should look like, before spans are merged.
#[derive(Clone, Copy, PartialEq, Eq)]
struct CellStyle {
    at_cursor: bool,
    /// The colour the mode paints its cursor in, when it has one. The plain
    /// cursor is a reverse video block; a coloured one takes the mode's colour
    /// as its background, the way the selection does.
    cursor_color: Option<(u8, u8, u8)>,
    /// Part of a search match, and whether it is the one the reader is on.
    match_here: bool,
    current_match: bool,
    /// Inside the range the vision mode has selected.
    selected: bool,
}

/// What every line of one frame is painted with, settled once: the room the
/// line has, the colours, and where the reader's eye, search and selection
/// stand. A frame draws many lines, and without this each one would carry
/// the same ten facts in as arguments — and a per-character pass would be
/// handed them again for every character.
struct FrameMarks<'a> {
    /// The width the line is set in.
    width: u16,
    theme: &'a crate::theme::Theme,
    search: &'a crate::search::Search,
    /// The cursor's place when the mode paints one.
    cursor: Option<(usize, usize)>,
    /// The colour the mode paints its cursor in, when it has one. The plain
    /// cursor is a reverse video block; a coloured one takes the mode's colour
    /// as its background, the way the selection does.
    cursor_color: Option<(u8, u8, u8)>,
    selection: Option<(usize, usize, usize, usize)>,
    focus: Option<usize>,
    title: Option<usize>,
}

fn draw_text(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme();
    let scroll = app.scroll();
    // The cursor wears its mode's colour: the cursor mode's green in the plain
    // view. A selection already paints blue under the cursor.
    let cursor_color = match app.mode {
        Mode::Cursor => Some(theme.marks[1]),
        _ => None,
    };
    // Everything the lines of this frame are painted with, gathered once:
    // the pass over the lines — and the per-character pass inside it — then
    // asks the app for nothing.
    let marks = FrameMarks {
        width: area.width,
        theme: &theme,
        search: app.search(),
        cursor: app.cursor_position(),
        cursor_color,
        selection: app.selection(),
        focus: app.focused_block(),
        title: app.chapter_title_block(),
    };

    let lines: Vec<Line> = app
        .visible_lines()
        .iter()
        .enumerate()
        .map(|(row, line)| {
            // A picture line is pixels, not text. Pixel protocols paint past
            // the text buffer, so their lines are left blank here and filled
            // in after the draw; a picture drawn as cells has rows to paint
            // here. A picture that has not arrived yet holds no pixels — one
            // dim line stands in the middle of its rows until it does.
            if let LineKind::Image { row: pixel_row, rows } = line.kind {
                if let Some(rendered) = app.image_at(line.block) {
                    return match rendered
                        .cells()
                        .and_then(|rows| rows.get(pixel_row as usize))
                    {
                        // A picture drawn as pixels paints past the buffer, so its
                        // lines stay blank here and the terminal fills them in;
                        // only a picture made of cells has rows to paint.
                        Some(cells) => image_row(cells, line.indent),
                        None => Line::from(""),
                    };
                }
                // The rows are reserved before the bytes are read, so a
                // picture on its way would otherwise leave a hole that fills
                // in under the reader. Nothing is said for the rows above and
                // below it, so the picture lands over this line without the
                // page moving.
                if pixel_row == rows / 2 && app.picture_pending(line.block) {
                    return placeholder(&app.picture_alt(line.block), line.block, &marks);
                }
            }
            render_line(line, scroll + row, &marks)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// The line a picture on its way stands in its place: its alt text — or the
/// dots where the book gave it none — centred in the room reserved for it,
/// wearing the style the picture's alt-text line wears and stepping back
/// with the page when it is out of focus, exactly like body text. The
/// picture lands over it on the same rows, so nothing moves when it does.
fn placeholder(alt: &str, block: usize, marks: &FrameMarks) -> Line<'static> {
    let label = if alt.is_empty() { "⋯" } else { alt };
    let indent = marks.width.saturating_sub(crate::measure::cells(label) as u16) / 2;
    let mut style = base_style(
        crate::doc::RunStyle::default(),
        LineKind::Image { row: 0, rows: 1 },
        marks.theme,
    );
    if !marks.focus.is_none_or(|focused| focused == block) {
        style = faded_style(style, marks.theme);
    }
    Line::from(vec![
        Span::raw(" ".repeat(indent as usize)),
        Span::styled(label.to_string(), style),
    ])
}

/// Paints one row of a picture. Each cell is four quadrants, painted in the
/// two colours the picture gave it: the bright quadrants are the foreground.
fn image_row(cells: &[crate::image::Cell], indent: u16) -> Line<'static> {
    let mut spans = Vec::with_capacity(cells.len() + 1);
    if indent > 0 {
        spans.push(Span::raw(" ".repeat(indent as usize)));
    }
    // Merge neighbouring cells that share a pattern and both colours, which
    // keeps the escape sequences short on the flat areas most illustrations
    // have.
    let mut current: Option<(crate::image::Cell, usize)> = None;
    for cell in cells {
        match &mut current {
            Some((last, count)) if last == cell => *count += 1,
            Some((last, count)) => {
                spans.push(quadrant(*last, *count));
                current = Some((*cell, 1));
            }
            None => current = Some((*cell, 1)),
        }
    }
    if let Some((cell, count)) = current {
        spans.push(quadrant(cell, count));
    }
    Line::from(spans)
}

fn quadrant(cell: crate::image::Cell, count: usize) -> Span<'static> {
    let (fr, fg, fb) = cell.fg;
    let (br, bg_, bb) = cell.bg;
    Span::styled(
        cell.ch.to_string().repeat(count),
        Style::default()
            .fg(Color::Rgb(fr, fg, fb))
            .bg(Color::Rgb(br, bg_, bb)),
    )
}

/// Draws one laid-out line, with the search, the cursor, the selection and
/// the focus painted over whatever the line already carries.
fn render_line(
    source: &SourceLine,
    line_index: usize,
    frame_marks: &FrameMarks,
) -> Line<'static> {
    let theme = frame_marks.theme;
    // The chapter's own title states the chapter, so it takes a colour of its
    // own; the headings below it wear the accent like every other heading.
    let is_title = frame_marks.title == Some(source.block);
    // The paragraph under the cursor keeps full colour; everything else
    // steps back into the page.
    let focused = frame_marks.focus.is_none_or(|block| block == source.block);
    let mut spans = Vec::new();
    if source.indent > 0 {
        spans.push(Span::raw(" ".repeat(source.indent as usize)));
    }

    // A listing's backdrop covers the indent as well, so the block has a clean
    // left edge.
    if source.kind == LineKind::Code {
        spans.clear();
        if source.indent > 0 {
            spans.push(Span::styled(
                " ".repeat(source.indent as usize),
                Style::default().bg(rgb(theme.code_background)),
            ));
        }
    }

    // A rule or a quote's marker is text like any other, so it steps back
    // with the paragraph it belongs to.
    let muted = if focused {
        Style::default().fg(rgb(theme.muted))
    } else {
        faded_style(Style::default().fg(rgb(theme.muted)), theme)
    };

    match source.kind {
        LineKind::Rule => {
            spans.push(Span::styled("* * *", muted));
            return Line::from(spans);
        }
        LineKind::Blank => return Line::from(""),
        LineKind::Quote => spans.push(Span::styled("▏ ", muted)),
        _ => {}
    }

    // Counts only real block text, so decoration never shifts an offset.
    let mut column = 0usize;
    // The search and the selection mean the same to every character on the
    // line, so they are settled once here rather than in the per-character path.
    let mut marks = LineMarks::new(frame_marks, source.block, line_index, source.offset);
    for piece in &source.pieces {
        let mut base = base_style(piece.style, source.kind, theme);
        if is_title {
            base = base.fg(title_color(theme));
        }
        let base = if focused { base } else { faded_style(base, theme) };
        if piece.decoration {
            spans.push(Span::styled(piece.text.clone(), base));
            continue;
        }
        // Split the piece wherever the cursor, a match or a selection starts
        // or ends.
        let mut current: Option<(CellStyle, String)> = None;
        for ch in piece.text.chars() {
            let cell = marks.cell_style_at(source.offset + column, column);
            match &mut current {
                Some((style, text)) if *style == cell => text.push(ch),
                Some((style, text)) => {
                    spans.push(styled_span(std::mem::take(text), base, *style, theme));
                    current = Some((cell, ch.to_string()));
                }
                None => current = Some((cell, ch.to_string())),
            }
            column += 1;
        }
        if let Some((style, text)) = current {
            spans.push(styled_span(text, base, style, theme));
        }
    }

    // A listing's backdrop runs to the right edge, so the block is a rectangle
    // rather than a ragged shape that follows the text.
    if source.kind == LineKind::Code {
        let used: usize = spans.iter().map(|s| cells(&s.content)).sum();
        if (used as u16) < frame_marks.width {
            spans.push(Span::styled(
                " ".repeat(frame_marks.width as usize - used),
                Style::default().bg(rgb(theme.code_background)),
            ));
        }
    }
    Line::from(spans)
}

/// The search and the selection as one line sees them, settled once per line:
/// the per-character pass over the line then only walks a cursor forwards and
/// compares two offsets.
struct LineMarks<'a> {
    /// The block the line is a slice of.
    block: usize,
    /// This block's matches, cut down to the ones that can reach this line.
    hits: &'a [(usize, usize)],
    /// How far a match runs from its start: the query, counted once.
    query_len: usize,
    /// The first match not yet painted. Matches are sorted and never overlap,
    /// so everything behind it has ended and is never looked at again.
    at: usize,
    /// The offsets on this line the selection covers, when it covers any.
    selection: Option<(usize, usize)>,
    /// The search the matches come from: which of them is the reader's is the
    /// same question for every character, so it is asked of the one search
    /// rather than handed down again per character.
    search: &'a crate::search::Search,
    /// The column the cursor sits on, when it sits on this line — its line is
    /// known before the line is walked, so all that is left to ask is which
    /// character it stands on.
    cursor: Option<usize>,
    /// The colour the mode paints the cursor in; see [`FrameMarks`].
    cursor_color: Option<(u8, u8, u8)>,
}

impl<'a> LineMarks<'a> {
    fn new(
        frame: &'a FrameMarks,
        block: usize,
        line: usize,
        line_offset: usize,
    ) -> Self {
        let search = frame.search;
        // Hits arrive sorted by (block, offset), so the block's own matches are
        // one binary cut and the ones that can reach this line a second: a match
        // begun above the line still paints its tail here, one that begins here
        // may run past its end, and only reaching past the line's first
        // character separates those from the matches wholly elsewhere.
        let query_len = search.query_len();
        let all = search.hits();
        let hits = &all[all.partition_point(|&(hit_block, _)| hit_block < block)..];
        let hits = &hits[..hits.partition_point(|&(hit_block, _)| hit_block <= block)];
        let hits = &hits[hits.partition_point(|&(_, start)| start + query_len <= line_offset)..];

        // Both ends of the selection are inside it, whichever way it was drawn;
        // against this line's block they collapse to one range of offsets, or to
        // nothing at all when the line lies outside them.
        let selection = frame.selection.and_then(|(start_block, start, end_block, end)| {
            if block < start_block || block > end_block {
                None
            } else {
                Some((
                    if block == start_block { start } else { 0 },
                    if block == end_block { end } else { usize::MAX },
                ))
            }
        });

        Self {
            block,
            hits,
            query_len,
            at: 0,
            selection,
            search,
            cursor: frame
                .cursor
                .filter(|&(at_line, _)| at_line == line)
                .map(|(_, column)| column),
            cursor_color: frame.cursor_color,
        }
    }

    /// What one character carries over the line's own style. The offset only
    /// grows while a line is painted, so the walk through the matches only ever
    /// moves forwards.
    fn cell_style_at(&mut self, offset: usize, column: usize) -> CellStyle {
        // A match spans the query length from its start, so the matches that
        // ended at or before this character can never cover it again.
        while self
            .hits
            .get(self.at)
            .is_some_and(|&(_, start)| start + self.query_len <= offset)
        {
            self.at += 1;
        }
        // Matches from one query never overlap, so the first match still
        // reaching past this character is the only one that can paint it —
        // and it paints it when it began at or before it.
        let start = self
            .hits
            .get(self.at)
            .map(|&(_, start)| start)
            .filter(|&start| start <= offset);
        CellStyle {
            at_cursor: self.cursor == Some(column),
            cursor_color: self.cursor_color,
            match_here: start.is_some(),
            // Only the hit's own start can name it the reader's match, so a
            // match spilling in from above is current only by where it began.
            current_match: start.is_some_and(|start| self.search.is_current(self.block, start)),
            selected: self
                .selection
                .is_some_and(|(from, to)| offset >= from && offset <= to),
        }
    }
}

/// The colour a chapter's own title takes: the blue a selection paints,
/// so the chapter's name reads as belonging to the chapter rather than to the
/// headings below it.
fn title_color(theme: &crate::theme::Theme) -> Color {
    rgb(theme.marks[2])
}

fn base_style(piece: crate::doc::RunStyle, kind: LineKind, theme: &crate::theme::Theme) -> Style {
    let mut style = Style::default();
    if piece.bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if piece.italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if piece.link {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if piece.code {
        style = style.fg(brightened(theme.code_foreground));
    }
    match kind {
        LineKind::Heading(1 | 2) => style.add_modifier(Modifier::BOLD).fg(rgb(theme.accent)),
        LineKind::Heading(_) => style.fg(rgb(theme.accent)),
        LineKind::Quote => style.fg(rgb(theme.quote)).add_modifier(Modifier::ITALIC),
        // A listing is set apart by a background of its own rather than by
        // colour alone, so it reads as one block even where lines are short.
        LineKind::Code => style
            .fg(rgb(theme.code_foreground))
            .bg(rgb(theme.code_background)),
        LineKind::Image { .. } => style.fg(rgb(theme.muted)).add_modifier(Modifier::ITALIC),
        // Body text has no colour of its own, and the terminal's default is
        // the theme's foreground at best: setting it here is what lets the
        // page be brighter than the chrome around it.
        _ => style.fg(brightened(theme.foreground)),
    }
}

fn styled_span(
    text: String,
    base: Style,
    cell: CellStyle,
    theme: &crate::theme::Theme,
) -> Span<'static> {
    let mut style = base;

    // A selection is what the vision mode has marked, so it paints under
    // everything else: a search match still stands out inside it, and the
    // cursor still shows at the end of it.
    if cell.selected {
        let (r, g, b) = theme.marks[2];
        let (fr, fg_, fb) = text_color_on((r, g, b), theme);
        style = style
            .bg(Color::Rgb(r, g, b))
            .fg(Color::Rgb(fr, fg_, fb));
    }

    // A match is what the reader is looking for right now, so it paints over
    // whatever else the cell holds. The one match the reader is on is bolded on
    // top, so it stands out among its siblings.
    if cell.match_here {
        let (r, g, b) = theme.accent;
        let (fr, fg_, fb) = text_color_on((r, g, b), theme);
        style = style.bg(Color::Rgb(r, g, b)).fg(Color::Rgb(fr, fg_, fb));
        if cell.current_match {
            style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        }
    }
    if cell.at_cursor {
        style = style.add_modifier(Modifier::BOLD);
        match cell.cursor_color {
            // A mode with a colour of its own paints the cursor in it, the way
            // the vision selection paints the text it covers.
            Some((r, g, b)) => {
                let (fr, fg_, fb) = text_color_on((r, g, b), theme);
                style = style.bg(Color::Rgb(r, g, b)).fg(Color::Rgb(fr, fg_, fb));
            }
            None => style = style.add_modifier(Modifier::REVERSED),
        }
    }
    Span::styled(text, style)
}

/// The status row turned into a prompt while a search is being typed: what
/// has been typed so far, and the block that says where the next character
/// lands.
fn prompt_line(input: &str, theme: &crate::theme::Theme) -> Line<'static> {
    Line::from(vec![
        Span::raw(" ".repeat(SIDE_MARGIN as usize)),
        Span::styled("/", Style::default().fg(rgb(theme.accent))),
        Span::styled(
            input.to_string(),
            Style::default().fg(rgb(theme.foreground)),
        ),
        // A block marks where the next character lands.
        Span::styled("▏", Style::default().fg(rgb(theme.accent))),
    ])
}

/// The status row itself: the message on the left in the muted colour, the
/// air between the two ends, and what the view has to say on the right in the
/// colour it was given.
///
/// The gap is the last thing to give way and never to nothing: two ends that
/// touch read as one run of text, so one cell of air is kept however little
/// room there is — which is why it is floored rather than allowed to close.
///
/// The ends come in as text rather than as `String`s: a fixed phrase the
/// terminal already holds — "q leaves" and the like — is passed through as it
/// is instead of being copied into a new `String` on every frame.
fn status_row(
    width: u16,
    theme: &crate::theme::Theme,
    left: impl Into<std::borrow::Cow<'static, str>>,
    right: impl Into<std::borrow::Cow<'static, str>>,
    right_style: Style,
) -> Line<'static> {
    let left = left.into();
    let right = right.into();
    let gap = width
        .saturating_sub(cells(&left) as u16 + cells(&right) as u16 + SIDE_MARGIN * 2)
        .max(1);
    Line::from(vec![
        Span::raw(" ".repeat(SIDE_MARGIN as usize)),
        Span::styled(left, Style::default().fg(rgb(theme.muted))),
        Span::raw(" ".repeat(gap as usize)),
        Span::styled(right, right_style),
    ])
}

fn draw_status(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme();

    // While a search is being typed, the status line is the prompt.
    if let Some(input) = app.search_input() {
        frame.render_widget(Paragraph::new(prompt_line(input, &theme)), area);
        return;
    }

    let (current, total) = app.chapter_number();

    // The latest word outranks the standing title: a message from what just
    // happened, then a link under the cursor, and only then the book itself.
    let left = match app.status() {
        Some(message) => message.to_string(),
        None if app.link_at_cursor().is_some() => {
            let link = app.link_at_cursor().expect("checked above");
            // A link comes from inside the book, and what the terminal is
            // given is what it obeys: the target is cleaned on the way out.
            i18n::fill(
                "link: {}  ·  Enter follows",
                &[&crate::journal::clean(&link.target)],
            )
        }
        None => format!("{}  ·  {}", app.title(), app.chapter_title()),
    };

    // The progress is what the row is for, so it is measured first and kept
    // whole: the position, the total and the percentage, which is what tells
    // the reader where they are. Everything else gives way to it.
    let progress = format!("{current}/{total}  {:>3}%", app.progress());
    let progress_width = cells(&progress);

    // The mode is a courtesy, kept while there is still room for a title with
    // more than the two characters and their mark that the floor leaves.
    let mode = mode_label(app.mode);
    // Two characters of title and the ellipsis that says more is there.
    const LEAST_TITLE: usize = 5;
    let before_title =
        (area.width as usize).saturating_sub(SIDE_MARGIN as usize * 2 + progress_width + 1);
    let mode = if before_title >= cells(mode) + LEAST_TITLE {
        mode
    } else {
        ""
    };
    let right = format!("{mode}{progress}");

    // The title takes what is left, cut with a mark when it does not fit, so it
    // can never push the progress off the row.
    let room = (area.width as usize).saturating_sub(SIDE_MARGIN as usize * 2 + cells(&right) + 1);
    let left = fit(&left, room);

    let mode_style = mode_color(app.mode, &theme);

    frame.render_widget(
        Paragraph::new(status_row(area.width, &theme, left, right, mode_style)),
        area,
    );
}

/// The mode indicator on the status row: the name, and the air after it that
/// keeps it from touching the title. Empty when no mode is named.
fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Cursor => "NORMAL  ",
        Mode::Vision => "VISION  ",
        _ => "",
    }
}

/// The colour of the mode indicator: the cursor in its green, the selection in
/// the blue it paints the page with.
fn mode_color(mode: Mode, theme: &crate::theme::Theme) -> Style {
    match mode {
        Mode::Cursor => Style::default().fg(rgb(theme.marks[1])),
        Mode::Vision => Style::default().fg(rgb(theme.marks[2])),
        _ => Style::default().fg(rgb(theme.muted)),
    }
}

fn centred(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(4));
    let height = height.min(area.height.saturating_sub(2)).max(3);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

fn draw_contents(frame: &mut Frame, area: Rect, app: &mut App) {
    let entries = app.contents();
    let panel = centred(area, 60, entries.len() as u16 + 2);
    let items: Vec<ListItem> = entries.into_iter().map(ListItem::new).collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(i18n::t(" Contents "))
                .border_style(Style::default().fg(rgb(app.theme().muted))),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    let mut state = ListState::default();
    state.select(Some(app.contents_cursor()));
    frame.render_widget(Clear, panel);
    frame.render_stateful_widget(list, panel, &mut state);
    // The list shifts itself to keep the cursor visible; the offset it settled
    // on is what turns a later click's row back into a chapter.
    app.set_contents_area(
        panel.x,
        panel.y,
        panel.width,
        panel.height,
        state.offset(),
    );
}

/// One row of a key list: the keys, then what they do.
fn binding_line(keys: &str, what: &str, theme: &crate::theme::Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("  {keys:<20}"),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(what.to_string(), Style::default().fg(rgb(theme.muted))),
    ])
}

/// Draws a key list in the panel both help screens share.
fn help_panel(frame: &mut Frame, area: Rect, width: u16, lines: Vec<Line<'static>>, theme: &crate::theme::Theme) {
    let panel = centred(area, width, lines.len() as u16 + 2);
    frame.render_widget(Clear, panel);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(i18n::t(" Keys - any key closes "))
                .border_style(Style::default().fg(rgb(theme.muted))),
        ),
        panel,
    );
}

/// Draws the key bindings, grouped as they are declared.
fn draw_help(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme();
    let mut lines: Vec<Line> = Vec::new();
    for (group, bindings) in crate::app::bindings() {
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            group,
            Style::default()
                .fg(rgb(theme.accent))
                .add_modifier(Modifier::BOLD),
        )));
        for (keys, what) in bindings {
            lines.push(binding_line(keys, what, &theme));
        }
    }
    help_panel(frame, area, 66, lines, &theme);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::LineKind;
    use crate::testkit::{key, text_book};
    use ratatui::crossterm::event::KeyCode;
    use unicode_width::UnicodeWidthChar;

    /// A book whose one chapter holds one flow picture: 64x64 pixels, which is
    /// 32 cells wide and 16 rows down at an 8x16 cell — too big to be a mark,
    /// and in the flow of text rather than a cover, so it is magnified into
    /// the room.
    fn picture_book(name: &str, title: &str) -> crate::testkit::TempBook {
        let art = crate::testkit::png(64, 64, [200, 100, 50, 255]);
        crate::testkit::TempBook::with(
            name,
            title,
            &[r#"<img src="art.png" alt="a diagram"/>"#],
            &[
                (
                    "OEBPS/content.opf",
                    &crate::testkit::opf(
                        title,
                        r#"<item id="art" href="art.png" media-type="image/png"/>"#,
                    ),
                ),
                ("OEBPS/art.png", &art),
            ],
        )
    }

    #[test]
    fn a_picture_is_centred_in_the_column_whichever_way_it_is_drawn() {
        // A picture is a figure on its own lines, so it stands in the middle
        // of the column rather than at its left edge: 64x64 pixels at an 8x16
        // cell is 32 cells wide and 16 rows tall, and 32 cells in the 36-cell
        // room of a 40-cell frame start 2 cells in. Both ways of drawing have
        // to agree on that column — the block fallback pads each row out to
        // it, and a pixel escape is emitted there — or one backend would draw
        // the same book differently from another.
        let book = picture_book("ui-centre", "Centre");
        for backend in [
            crate::image::Backend::Quad,
            crate::image::Backend::Kitty,
            crate::image::Backend::Sixel,
        ] {
            let (_dir, mut app) =
                crate::testapp::opened_default(&format!("ui-centre-journal-{backend:?}"), &book);
            app.set_image_backend(backend);

            let (buffer, placed) = crate::testapp::frame_with_placements(&mut app, 40, 24);

            let picture: Vec<&SourceLine> = app
                .visible_lines()
                .iter()
                .filter(|line| matches!(line.kind, LineKind::Image { .. }))
                .collect();
            assert_eq!(picture.len(), 16, "{backend:?}: the picture is 16 rows");

            // The column the picture actually starts at: the text area sits
            // two cells in from the frame, so the offset lands at 4.
            let drawn_at = |y: u16| {
                (0..40u16).find(|x| buffer.cell((*x, y)).map(|cell| cell.symbol()) == Some("█"))
            };
            match backend {
                // Blocks are written as cells, so the offset is blank cells in
                // front of every row of the picture. The fixture is one flat
                // colour, so every quadrant of every cell is lit and the glyph
                // is a full block.
                crate::image::Backend::Quad => {
                    assert_eq!(drawn_at(0), Some(4), "the cells start at the offset");
                    assert!(placed.is_empty(), "a block picture places no escape");
                }
                // A pixel protocol paints past the buffer, so the escape is
                // emitted at the offset column instead.
                _ => {
                    assert_eq!(drawn_at(0), None, "pixels are not cells");
                    assert_eq!(placed.len(), 1, "{backend:?}: one escape, one picture");
                    assert_eq!(
                        placed[0].column, 4,
                        "{backend:?}: the escape moves to the offset column first"
                    );
                    assert_eq!(placed[0].row, 0, "the picture starts on its own row");
                }
            }
        }

        // And a frame too small to hold it: an image does not only grow into
        // the room, it shrinks into a window smaller than itself rather than
        // vanishing from it.
        let (_dir, mut app) = crate::testapp::opened_default("ui-shrink-journal", &book);
        app.set_image_backend(crate::image::Backend::Quad);
        for (width, height) in [(40u16, 12u16), (12, 6), (5, 4)] {
            crate::testapp::frame(&mut app, width, height);
            let drawn = app
                .visible_lines()
                .iter()
                .filter(|line| matches!(line.kind, LineKind::Image { .. }))
                .count();
            assert!(drawn >= 1, "{width}x{height}: the picture is still there");
        }
    }

    /// Presses `j` until the view stands where `here` says it should. A scroll
    /// position is named by what the reader sees, because where the top of the
    /// page lands is the cursor's line minus half the view, and neither of
    /// those is the picture's own row.
    fn scrolled_to(app: &mut App, here: impl Fn(&[SourceLine]) -> bool) {
        for _ in 0..400 {
            if here(app.visible_lines()) {
                return;
            }
            // One row a press: this helper aims at an exact view, and a rush
            // would step over the very row it is aiming for.
            app.reset_rush();
            app.handle_key(key(KeyCode::Char('j')));
        }
        panic!("the view never stood where the test asked");
    }

    /// One place the view can stand on the picture: how the page has to be
    /// scrolled for it, the room it is drawn in, and what the placement must
    /// say — the row it starts on and the rows of the picture it shows.
    type Scene = (&'static str, fn(&[SourceLine]) -> bool, Rect, (u16, (u16, u16)));

    /// The view three lines into the picture: its first row still on the
    /// page and its bottom cut off by the room below it.
    fn three_lines_in(lines: &[SourceLine]) -> bool {
        matches!(
            lines.get(3).map(|line| line.kind),
            Some(LineKind::Image { row: 0, .. })
        )
    }

    /// The view with the picture's first row at the top of the page.
    fn at_the_top(lines: &[SourceLine]) -> bool {
        matches!(
            lines.first().map(|line| line.kind),
            Some(LineKind::Image { row: 0, .. })
        )
    }

    /// The view five rows into the picture: its top scrolled away.
    fn five_rows_in(lines: &[SourceLine]) -> bool {
        matches!(
            lines.first().map(|line| line.kind),
            Some(LineKind::Image { row: 5, .. })
        )
    }

    /// A book with a picture big enough to be cut by any view: text before
    /// it to scroll up with, and more text after it than it has rows, so the
    /// page can stand past its last row.
    fn crop_book() -> crate::testkit::TempBook {
        let before = "the lines that stand before the picture in this little book of ours, set long enough to wrap several times over";
        let after = "and after it, more lines than the picture has rows below it, so the page may scroll past the picture's last row and still have somewhere to stand: a paragraph of some length, a paragraph of some length, a paragraph of some length, a paragraph of some length, a paragraph of some length, a paragraph of some length, a paragraph of some length";
        crate::testkit::TempBook::with(
            "ui-crop-pixel",
            "Crop",
            &[&format!(
                "<p>{before}</p><img src=\"pic.png\" alt=\"a diagram\"/><p>{after}</p>"
            )],
            &[
                (
                    "OEBPS/content.opf",
                    &crate::testkit::opf(
                        "Crop",
                        r#"<item id="pic" href="pic.png" media-type="image/png"/>"#,
                    ),
                ),
                (
                    "OEBPS/pic.png",
                    &crate::testkit::png(64, 52, [200, 100, 50, 255]),
                ),
            ],
        )
    }

    #[test]
    fn a_pixel_picture_is_drawn_down_to_where_the_view_ends() {
        // The bug this guards: a picture that could not fit the text area
        // whole was not drawn at all, and one whose top had scrolled away
        // disappeared with it. A pixel protocol paints past the cells, so
        // nothing clips it — the view has to crop the picture itself and say
        // which of its rows are visible. 64x52 pixels at an 8x16 cell is 13
        // rows in the room of this 40x14 frame.
        let book = crop_book();
        // Where the view may stand on the picture — its bottom cut off, all
        // of it, or its top scrolled away — and what the placement has to say
        // about it. The crop is the view's own arithmetic and does not depend
        // on the protocol, so the three shapes are pinned on kitty; sixel
        // takes the first to show it draws the same way.
        let scenes: [Scene; 3] = [
            ("seven rows show", three_lines_in, Rect::new(0, 0, 40, 10), (3, (0, 7))),
            ("every row, none cut", at_the_top, Rect::new(0, 0, 40, 20), (0, (0, 13))),
            ("the eight rows that are left", five_rows_in, Rect::new(0, 0, 40, 10), (0, (5, 8))),
        ];
        for backend in [crate::image::Backend::Kitty, crate::image::Backend::Sixel] {
            let (_dir, mut app) =
                crate::testapp::opened_default(&format!("ui-crop-pixel-{backend:?}"), &book);
            app.set_image_backend(backend);
            app.prepare(40, 14);
            let block = app
                .visible_lines()
                .iter()
                .find_map(|line| matches!(line.kind, LineKind::Image { .. }).then_some(line.block))
                .expect("the picture stands on the first page");
            assert_eq!(
                app.image_at(block).map(crate::image::Rendered::height),
                Some(13),
                "{backend:?}: the fixture is thirteen rows"
            );

            let asked = if backend == crate::image::Backend::Kitty {
                &scenes[..]
            } else {
                &scenes[..1]
            };
            for (name, stands, room, want) in asked {
                scrolled_to(&mut app, stands);
                let placed = placements(*room, &app);
                assert_eq!(placed.len(), 1, "{backend:?}: one picture, one placement");
                assert_eq!(
                    (placed[0].row, placed[0].crop),
                    *want,
                    "{backend:?}: {name}"
                );
            }
        }
    }

    #[test]
    fn the_progress_survives_a_long_title_in_a_narrow_frame() {
        // The bug this guards: a title longer than the row used to run on and
        // push the progress off the right edge, so a small window showed where
        // you were in the book as nothing at all.
        let book = picture_book("ui-progress", "The Long And Winding Book Title");
        let (_dir, mut app) =
            crate::testapp::opened_default("ui-progress-journal", &book);

        // The narrowest frame that still carries a status row.
        let buffer = crate::testapp::frame(&mut app, STATUS_MIN_WIDTH, 6);
        let status: String = (0..STATUS_MIN_WIDTH)
            .map(|x| buffer.cell((x, 5)).map(|c| c.symbol()).unwrap_or(" "))
            .collect();
        assert!(status.contains('/'), "the position is kept: {status:?}");
        assert!(status.contains('%'), "the percentage is kept: {status:?}");
        // The title is what gives way, with the mark that says it did.
        assert!(
            status.contains('…'),
            "the title is cut, not dropped: {status:?}"
        );
    }

    /// Asserts that what the app calls the focus is exactly what the buffer
    /// paints at full colour, on every text row of the page.
    fn focus_is_what_the_buffer_paints(app: &App, buffer: &ratatui::buffer::Buffer) {
        let theme = app.theme();
        let focused = app.focused_block().expect("a paragraph is in focus");
        let title = app.chapter_title_block();
        let mut saw_focus = false;
        let mut saw_dimmed = false;
        for (row, line) in app.visible_lines().iter().enumerate() {
            if line.kind == LineKind::Blank {
                continue;
            }
            let style = line
                .pieces
                .first()
                .map(|piece| piece.style)
                .unwrap_or_default();
            let mut base = base_style(style, line.kind, &theme);
            if title == Some(line.block) {
                base = base.fg(title_color(&theme));
            }
            let expected = if line.block == focused {
                base.fg
            } else {
                faded_style(base, &theme).fg
            }
            .unwrap_or(Color::Reset);
            // The cursor paints the character it sits on in its own colour, so
            // the row's own colour is checked at the next character along.
            let column = if app.cursor_position() == Some((app.scroll() + row, 0)) {
                1
            } else {
                0
            };
            let cell = buffer
                .cell((margin(buffer.area.width) + line.indent + column, row as u16))
                .unwrap();
            assert_eq!(cell.fg, expected, "row {row} (block {})", line.block);
            saw_focus |= line.block == focused;
            saw_dimmed |= line.block != focused;
        }
        assert!(saw_focus && saw_dimmed, "the page shows both sides of the focus");
    }

    /// Whether the page holds a reverse-video cell, which is how a cursor with
    /// no colour of its own is painted.
    fn paints_a_cursor(buffer: &ratatui::buffer::Buffer) -> bool {
        (0..buffer.area.height).any(|y| {
            (0..buffer.area.width).any(|x| {
                buffer
                    .cell((x, y))
                    .unwrap()
                    .modifier
                    .contains(Modifier::REVERSED)
            })
        })
    }

    #[test]
    fn the_hidden_cursor_carries_the_focus_and_is_not_drawn() {
        // Reading draws no cursor, but the page still rests on one: it starts
        // on the first line and walks down as the reader scrolls. That is how
        // the first paragraph — and, at the end of a chapter, the last — gets
        // to be the focus like any other.
        let book = text_book("ui-focus", "Focus", 12);
        let (_dir, mut app) = crate::testapp::opened_default("ui-focus-journal", &book);

        let buffer = crate::testapp::frame(&mut app, 40, 8);
        assert_eq!(app.focused_block(), Some(0), "the first line is the cursor");
        focus_is_what_the_buffer_paints(&app, &buffer);
        assert!(!paints_a_cursor(&buffer), "reading draws no cursor");

        // Scrolling moves the hidden cursor into the chapter, and the page
        // follows it.
        for _ in 0..4 {
            app.handle_key(key(KeyCode::Char('j')));
        }
        let buffer = crate::testapp::frame(&mut app, 40, 8);
        assert!(app.focused_block().unwrap() > 0, "the cursor walked on");
        assert!(app.scroll() > 0, "and the page followed it");
        focus_is_what_the_buffer_paints(&app, &buffer);

        // Leaving the cursor mode leaves nothing behind: the cursor still
        // holds the reading position, but the page no longer paints it.
        app.handle_key(key(KeyCode::Char('i')));
        crate::testapp::frame(&mut app, 40, 8);
        app.handle_key(key(KeyCode::Esc));
        let buffer = crate::testapp::frame(&mut app, 40, 8);
        assert!(app.cursor_position().is_none(), "the cursor is hidden again");
        assert!(
            !paints_a_cursor(&buffer),
            "and nothing of it is left on the page"
        );

    }

    #[test]
    fn the_mode_indicator_names_the_mode_in_its_own_colour() {
        // The bottom-right corner says which mode the reader is in, in that
        // mode's colour: the cursor in its green, the selection in the blue
        // it paints the page with, and no word for the plain reading view.
        let theme = crate::theme::Theme::default();
        assert_eq!(mode_label(Mode::Cursor), "NORMAL  ");
        assert_eq!(mode_label(Mode::Vision), "VISION  ");
        assert_eq!(mode_label(Mode::Reading), "");
        assert_eq!(mode_color(Mode::Cursor, &theme).fg, Some(rgb(theme.marks[1])));
        assert_eq!(mode_color(Mode::Vision, &theme).fg, Some(rgb(theme.marks[2])));
        assert_eq!(mode_color(Mode::Reading, &theme).fg, Some(rgb(theme.muted)));
    }

    #[test]
    fn the_normal_cursor_is_the_green_of_its_mode() {
        // A mode with a colour of its own paints the cursor in it, with the
        // text colour that reads on it; without one the cursor stays the
        // plain reverse video block.
        let theme = crate::theme::Theme::default();
        let green = theme.marks[1];
        let cell = CellStyle {
            at_cursor: true,
            cursor_color: Some(green),
            match_here: false,
            current_match: false,
            selected: false,
        };
        let span = styled_span("x".to_string(), Style::default(), cell, &theme);
        assert_eq!(span.style.bg, Some(rgb(green)));
        assert_eq!(span.style.fg, Some(rgb(text_color_on(green, &theme))));
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
        assert!(!span.style.add_modifier.contains(Modifier::REVERSED));

        let cell = CellStyle {
            cursor_color: None,
            ..cell
        };
        let span = styled_span("x".to_string(), Style::default(), cell, &theme);
        assert!(span.style.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn the_chapters_own_title_is_blue_and_the_headings_below_are_not() {
        // The chapter's name takes the selection blue; the headings under it
        // keep the accent every other heading wears. One screen shows both:
        // the title opens the chapter, and the accent is checked where the
        // headings are styled.
        let book = text_book("ui-title", "Chapter Name", 12);
        let (_dir, mut app) = crate::testapp::opened_default("ui-title-journal", &book);

        let buffer = crate::testapp::frame(&mut app, 40, 8);
        assert_eq!(app.chapter_title_block(), Some(0));
        assert_eq!(
            buffer.cell((2, 0)).unwrap().fg,
            title_color(&app.theme()),
            "the title is painted in its own blue"
        );
        let theme = app.theme();
        assert_eq!(
            base_style(
                crate::doc::RunStyle::default(),
                LineKind::Heading(2),
                &theme
            )
            .fg,
            Some(rgb(theme.accent)),
            "a heading below the title keeps the accent"
        );

    }

    #[test]
    fn in_cursor_mode_the_cursor_paragraph_is_the_one_not_dimmed() {
        // The paragraph under the cursor keeps full colour wherever it sits
        // on the page; the rest step back.
        let book = text_book("ui-cursor-focus", "Cursor", 12);
        let (_dir, mut app) = crate::testapp::opened_default("ui-cursor-focus-journal", &book);
        crate::testapp::frame(&mut app, 40, 8);
        // A cursor needs a laid-out page to stand on; the reader always draws
        // before a key arrives, and the test does too.
        app.handle_key(key(KeyCode::Char('i')));
        let buffer = crate::testapp::frame(&mut app, 40, 8);

        assert!(
            app.cursor_position().is_some(),
            "the cursor mode shows its cursor"
        );
        focus_is_what_the_buffer_paints(&app, &buffer);

    }

    #[test]
    fn the_search_match_keeps_the_accent_over_a_line_break() {
        // A match belongs to the block, but the page shows wrapped lines, so
        // the accent has to follow it over the break — the passage found here
        // is cut by the wrap — and touch nothing around it: every other
        // character on the page keeps the background it came with.
        let book = text_book("ui-search", "Search", 12);
        let (_dir, mut app) = crate::testapp::opened_default("ui-search-journal", &book);
        app.search_for("to wrap".into());

        let buffer = crate::testapp::frame(&mut app, 40, 8);
        let accent = rgb(app.theme().accent);
        let query_len = app.search().query_len();
        let hits = app.search().hits().to_vec();
        assert!(!hits.is_empty(), "the passage is found: {hits:?}");

        // Walk the page the way it was drawn: each character to its offset in
        // the block and its cell on the row. A cell is only worth checking
        // when it stands at a match's edge — the match's own characters and
        // one on either side — where the accent has to start and stop.
        let mut rows_per_hit: Vec<std::collections::BTreeSet<usize>> = hits
            .iter()
            .map(|_| std::collections::BTreeSet::new())
            .collect();
        let mut matched = 0usize;
        for (row, line) in app.visible_lines().iter().enumerate() {
            let mut offset = line.offset;
            let mut column = 0usize;
            for piece in &line.pieces {
                for ch in piece.text.chars() {
                    let x = margin(buffer.area.width) + line.indent + column as u16;
                    if !piece.decoration {
                        // Only this line's block can paint it, and a match runs
                        // the query length from where it starts.
                        let hit = hits.iter().position(|&(hit_block, start)| {
                            hit_block == line.block && offset >= start && offset < start + query_len
                        });
                        let edge = hits.iter().any(|&(hit_block, start)| {
                            hit_block == line.block
                                && offset + 1 >= start
                                && offset <= start + query_len
                        });
                        if edge {
                            let cell = buffer.cell((x, row as u16)).unwrap();
                            assert_eq!(
                                cell.bg == accent,
                                hit.is_some(),
                                "row {row} column {x} ({ch:?}): the cell is {}, a match says {}",
                                cell.bg == accent,
                                hit.is_some()
                            );
                        }
                        if let Some(hit) = hit {
                            matched += 1;
                            rows_per_hit[hit].insert(row);
                        }
                        offset += 1;
                    }
                    column += ch.width().unwrap_or(0);
                }
            }
        }
        assert!(matched > 0, "the match is painted on the page");
        assert!(
            rows_per_hit.iter().any(|rows| rows.len() > 1),
            "one match is painted on two rows: the wrap cut through it"
        );

    }

    #[test]
    fn a_picture_on_its_way_holds_its_rows_with_a_placeholder() {
        // The layout reserves the picture's rows before its bytes are read,
        // and while the decode is on its way they used to be blank: the page
        // showed a hole that filled in under the reader. One centred, dim
        // line — the alt text, or dots where the book gave none — names what
        // is coming, and goes without a row moving once the picture lands.
        // The jobs wait in a queue, so the test holds the picture on its way
        // for as long as it likes.
        let nameless = crate::testkit::TempBook::with(
            "ui-pending-nameless",
            "Nameless",
            &[r#"<img src="art.png"/>"#],
            &[
                (
                    "OEBPS/content.opf",
                    &crate::testkit::opf(
                        "Nameless",
                        r#"<item id="art" href="art.png" media-type="image/png"/>"#,
                    ),
                ),
                (
                    "OEBPS/art.png",
                    &crate::testkit::png(64, 64, [200, 100, 50, 255]),
                ),
            ],
        );
        // The room is 36 cells wide inside the margins, so a label is
        // centred at margin + (36 - its own width) / 2 — which for the
        // nine-cell alt text is column 15. A picture the book gave no alt
        // text still carries words: the path its bytes live under, which is
        // what the layout's own fallback line shows for it.
        for (tag, book, label) in [
            (
                "ui-pending",
                picture_book("ui-pending", "Pending"),
                "a diagram",
            ),
            ("ui-pending-nameless", nameless, "OEBPS/art.png"),
        ] {
            let (_dir, mut app) = crate::testapp::opened_default(&format!("{tag}-journal"), &book);
            app.set_image_backend(crate::image::Backend::Quad);
            app.defer_picture_jobs();

            // The decode waits in the queue: the frame shows the rows the
            // layout reserved, with the placeholder alone on the middle one.
            let buffer = crate::testapp::frame(&mut app, 40, 24);
            let drawn: Vec<u16> = (0..16u16)
                .filter(|&y| {
                    (0..40u16).any(|x| {
                        buffer
                            .cell((x, y))
                            .is_some_and(|cell| cell.symbol() != " ")
                    })
                })
                .collect();
            assert_eq!(drawn, vec![8], "{tag}: the middle row alone says something");
            let row: String = (0..40u16)
                .map(|x| buffer.cell((x, 8)).map_or(" ", |cell| cell.symbol()))
                .collect();
            let at = row
                .find(label)
                .unwrap_or_else(|| panic!("{tag}: the placeholder is on the middle row: {row:?}")) as u16;
            assert_eq!(
                at,
                margin(40) + (36 - crate::measure::cells(label) as u16) / 2,
                "{tag}: the placeholder is centred in the column"
            );
            let cell = buffer.cell((at, 8)).unwrap();
            let theme = app.theme();
            assert_eq!(
                cell.fg,
                rgb(theme.muted),
                "{tag}: it wears the picture line's colour"
            );
            assert!(
                cell.modifier.contains(Modifier::ITALIC),
                "{tag}: and its italic"
            );

            // The picture lands: the same rows hold its cells, and the
            // placeholder is gone without one of them moving.
            let buffer = crate::testapp::frame(&mut app, 40, 24);
            assert!(
                (0..40u16).all(|x| buffer.cell((x, 8)).is_some_and(|cell| cell.symbol() != label)),
                "{tag}: the placeholder made way for the picture"
            );
            assert_eq!(
                buffer.cell((4, 8)).map(|cell| cell.symbol()),
                Some("█"),
                "{tag}: the picture's own cells are on the very rows"
            );
        }
    }

    #[test]
    fn a_placeholder_with_no_words_to_say_is_the_dots() {
        // Every image block the parser makes carries words — the alt text,
        // or the path the bytes live under — but the view still names the
        // dots when a block arrives with none, so a reserved row is never
        // silent. Drawn straight, because no book can produce such a block.
        let theme = crate::theme::Theme::default();
        let search = crate::search::Search::default();
        let marks = FrameMarks {
            width: 36,
            theme: &theme,
            search: &search,
            cursor: None,
            cursor_color: None,
            selection: None,
            focus: None,
            title: None,
        };

        let line = placeholder("", 0, &marks);
        assert_eq!(
            line.spans[0].content.as_ref(),
            " ".repeat(17).as_str(),
            "the dots are centred in the column"
        );
        assert_eq!(line.spans[1].content.as_ref(), "⋯");
        assert_eq!(line.spans[1].style.fg, Some(rgb(theme.muted)));
        assert!(line.spans[1].style.add_modifier.contains(Modifier::ITALIC));

        // Out of focus it steps back with the page, like any body text:
        // its colour blended toward what lies behind, not left at full muted.
        let marks = FrameMarks {
            focus: Some(9),
            ..marks
        };
        let line = placeholder("a diagram", 0, &marks);
        assert!(
            line.spans[1].style.fg.is_some_and(|fg| fg != rgb(theme.muted)),
            "a placeholder out of focus wears the focus fade"
        );
    }
}

