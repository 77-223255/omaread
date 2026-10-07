//! Drawing the reader.

use crate::app::{App, Mode};
use crate::i18n;
use crate::layout::{Line as SourceLine, LineKind};
use crate::theme::text_color_on;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Columns kept free left and right of the text.
const SIDE_MARGIN: u16 = 2;

fn rgb((r, g, b): crate::theme::Rgb) -> Color {
    Color::Rgb(r, g, b)
}

/// Where a picture goes on screen, for the backends that paint past the text
/// buffer. Collected during the draw and written afterwards.
#[derive(Debug, Clone)]
pub struct Placement {
    /// Column and row on screen, zero-based.
    pub column: u16,
    pub row: u16,
    pub escape: String,
}

/// The frame split into the text area above and the one status row below.
///
/// The split is built from `app::text_rows` — the reader's own count of the
/// rows a picture may have — so the two cannot drift apart: one definition,
/// and a picture whose room is taller than the area it is drawn in cannot
/// run under the status line.
pub fn split_frame(area: Rect) -> (Rect, Rect) {
    let text_height = crate::app::text_rows(area.height);
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

pub fn draw(frame: &mut Frame, app: &mut App) -> Vec<Placement> {
    let (text_area, status_area) = split_frame(frame.area());

    let inner = Rect {
        x: text_area.x + SIDE_MARGIN,
        y: text_area.y,
        width: text_area.width.saturating_sub(SIDE_MARGIN * 2),
        height: text_area.height,
    };

    // The whole frame, not the text area: the reader takes the status row off
    // itself, so the room for pictures and the rows the layout reserves for
    // them are worked out in one place, from one height.
    app.prepare(inner.width, frame.area().height);
    draw_text(frame, inner, app);
    draw_status(frame, status_area, app);

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
/// one starts at. Only their first line matters: the protocol paints downwards
/// from there.
fn placements(area: Rect, app: &App) -> Vec<Placement> {
    let mut out = Vec::new();
    for (row, line) in app.visible_lines().iter().enumerate() {
        let LineKind::Image { row: pixel_row, .. } = line.kind else {
            continue;
        };
        // Only the top row of a picture places it.
        if pixel_row != 0 {
            continue;
        }
        let Some(rendered) = app.image_at(line.block) else {
            continue;
        };
        let Some(escape) = rendered.escape() else {
            continue;
        };
        if escape.is_empty() {
            continue;
        }
        // The terminal paints these pixels itself, so nothing clips them at the
        // edge of the text: a picture whose lower half is below the fold would
        // cover the status line. It waits until the whole of it fits, which is
        // one more line of scrolling. Half block pictures are ordinary cells and
        // never get here.
        if row + rendered.height() > area.height as usize {
            continue;
        }
        out.push(Placement {
            column: area.x + line.indent,
            row: area.y + row as u16,
            escape: escape.to_string(),
        });
    }
    out
}

/// What a single character should look like, before spans are merged.
#[derive(Clone, Copy, PartialEq, Eq)]
struct CellStyle {
    at_cursor: bool,
    /// Part of a search match, and whether it is the one the reader is on.
    match_here: bool,
    current_match: bool,
}

fn draw_text(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme();
    let scroll = app.scroll();
    let cursor = app.cursor_position();
    let search = app.search();

    let lines: Vec<Line> = app
        .visible_lines()
        .iter()
        .enumerate()
        .map(|(row, line)| match line.kind {
            // A picture line is pixels, not text. Pixel protocols paint past the
            // text buffer, so their lines are left blank here and filled in after
            // the draw.
            LineKind::Image { row: pixel_row, .. } => match app.image_at(line.block) {
                Some(rendered) => match rendered.cells() {
                    Some(_) => image_row(rendered, pixel_row, line.indent),
                    None => Line::from(""),
                },
                None => render_line(line, scroll + row, cursor, area.width, &theme, search),
            },
            _ => render_line(line, scroll + row, cursor, area.width, &theme, search),
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// Paints one row of a picture. Each cell shows the upper half block, with the
/// foreground carrying the upper pixel and the background the lower one.
fn image_row(rendered: &crate::image::Rendered, row: u16, indent: u16) -> Line<'static> {
    let Some(cells) = rendered.cells().and_then(|rows| rows.get(row as usize)) else {
        return Line::from("");
    };
    let mut spans = Vec::with_capacity(cells.len() + 1);
    if indent > 0 {
        spans.push(Span::raw(" ".repeat(indent as usize)));
    }
    // Merge neighbouring cells that share both colours, which keeps the escape
    // sequences short on the flat areas most illustrations have.
    let mut current: Option<(crate::image::Cell, usize)> = None;
    for cell in cells {
        match &mut current {
            Some((last, count)) if last == cell => *count += 1,
            Some((last, count)) => {
                spans.push(half_block(*last, *count));
                current = Some((*cell, 1));
            }
            None => current = Some((*cell, 1)),
        }
    }
    if let Some((cell, count)) = current {
        spans.push(half_block(cell, count));
    }
    Line::from(spans)
}

fn half_block(cell: crate::image::Cell, count: usize) -> Span<'static> {
    let (ur, ug, ub) = cell.upper;
    let (lr, lg, lb) = cell.lower;
    Span::styled(
        "▀".repeat(count),
        Style::default()
            .fg(Color::Rgb(ur, ug, ub))
            .bg(Color::Rgb(lr, lg, lb)),
    )
}

fn render_line(
    source: &SourceLine,
    line_index: usize,
    cursor: Option<(usize, usize)>,
    width: u16,
    theme: &crate::theme::Theme,
    search: &crate::search::Search,
) -> Line<'static> {
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

    match source.kind {
        LineKind::Rule => {
            spans.push(Span::styled("* * *", Style::default().fg(rgb(theme.muted))));
            return Line::from(spans);
        }
        LineKind::Blank => return Line::from(""),
        LineKind::Quote => spans.push(Span::styled("▏ ", Style::default().fg(rgb(theme.muted)))),
        _ => {}
    }

    // Counts only real block text, so decoration never shifts an offset.
    let mut column = 0usize;
    for piece in &source.pieces {
        let base = base_style(piece.style, source.kind, theme);
        if piece.decoration {
            spans.push(Span::styled(piece.text.clone(), base));
            continue;
        }
        // Split the piece wherever the cursor or a match starts or ends.
        let mut current: Option<(CellStyle, String)> = None;
        for ch in piece.text.chars() {
            let cell = cell_style_at(
                source.block,
                source.offset + column,
                line_index,
                column,
                cursor,
                search,
            );
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
        if (used as u16) < width {
            spans.push(Span::styled(
                " ".repeat(width as usize - used),
                Style::default().bg(rgb(theme.code_background)),
            ));
        }
    }
    Line::from(spans)
}

fn cell_style_at(
    block: usize,
    offset: usize,
    line: usize,
    column: usize,
    cursor: Option<(usize, usize)>,
    search: &crate::search::Search,
) -> CellStyle {
    // A match spans the length of the query from its start.
    let length = search.len();
    let hit = search.hits().iter().rev().find(|(hit_block, start)| {
        *hit_block == block && offset >= *start && offset < start + length
    });
    CellStyle {
        at_cursor: cursor == Some((line, column)),
        match_here: hit.is_some(),
        current_match: hit.is_some_and(|(b, start)| search.is_current(*b, *start)),
    }
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
        style = style.fg(rgb(theme.code_foreground));
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
        _ => style,
    }
}

fn styled_span(
    text: String,
    base: Style,
    cell: CellStyle,
    theme: &crate::theme::Theme,
) -> Span<'static> {
    let mut style = base;

    // A match is what the reader is looking for right now, so it paints over
    // whatever else the cell holds. The one match the reader is on is bolded on
    // top, so it stands out among its siblings.
    if cell.match_here {
        let (r, g, b) = theme.accent;
        let (fr, fg_, fb) = text_color_on((r, g, b));
        style = style.bg(Color::Rgb(r, g, b)).fg(Color::Rgb(fr, fg_, fb));
        if cell.current_match {
            style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        }
    }
    if cell.at_cursor {
        style = style
            .add_modifier(Modifier::REVERSED)
            .add_modifier(Modifier::BOLD);
    }
    Span::styled(text, style)
}

fn draw_status(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme();

    // While a search is being typed, the status line is the prompt.
    if let Some(input) = app.search_input() {
        let line = Line::from(vec![
            Span::raw(" ".repeat(SIDE_MARGIN as usize)),
            Span::styled("/", Style::default().fg(rgb(theme.accent))),
            Span::styled(
                input.to_string(),
                Style::default().fg(rgb(theme.foreground)),
            ),
            // A block marks where the next character lands.
            Span::styled("▏", Style::default().fg(rgb(theme.accent))),
        ]);
        frame.render_widget(Paragraph::new(line), area);
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

    let mode = match app.mode {
        Mode::Normal => "NORMAL  ",
        _ => "",
    };
    let right = format!("{mode}{current}/{total}  {:>3}%", app.progress());

    let gap = area
        .width
        .saturating_sub(cells(&left) as u16 + cells(&right) as u16 + SIDE_MARGIN * 2)
        .max(1);

    let mode_style = match app.mode {
        Mode::Normal => Style::default().fg(rgb(theme.marks[1])),
        _ => Style::default().fg(rgb(theme.muted)),
    };

    let line = Line::from(vec![
        Span::raw(" ".repeat(SIDE_MARGIN as usize)),
        Span::styled(left, Style::default().fg(rgb(theme.muted))),
        Span::raw(" ".repeat(gap as usize)),
        Span::styled(right, mode_style),
    ]);
    frame.render_widget(Paragraph::new(line), area);
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

fn draw_contents(frame: &mut Frame, area: Rect, app: &App) {
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
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {keys:<20}"),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(what, Style::default().fg(rgb(theme.muted))),
            ]));
        }
    }

    let height = lines.len() as u16 + 2;
    let panel = centred(area, 66, height);
    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(i18n::t(" Keys - any key closes "))
            .border_style(Style::default().fg(rgb(theme.muted))),
    );
    frame.render_widget(Clear, panel);
    frame.render_widget(paragraph, panel);
}

fn shorten(text: &str, width: usize) -> String {
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    cut(&collapsed, width)
}

/// How many cells a line of text takes on screen.
///
/// Not its character count: a Chinese character, a fullwidth comma, a bracket
/// from a CJK font all hold two cells, and a row laid out as if they held one
/// drifts a cell to the right for each of them. The East Asian ambiguous ones —
/// `…`, `“”`, `·` — hold one, which is what a terminal draws.
pub fn cells(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// The text cut to fit a number of cells, with a mark where it was cut.
fn cut(text: &str, width: usize) -> String {
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
    out.push('…');
    out
}

// ----- the library view -----

/// Draws the shelf: one row per book, with a filter prompt when one is being
/// typed.
pub fn draw_shelf(frame: &mut Frame, shelf: &mut crate::shelf::Shelf, theme: &crate::theme::Theme) {
    let (list_area, status_area) = split_frame(frame.area());

    let inner = Rect {
        x: list_area.x + SIDE_MARGIN,
        y: list_area.y,
        width: list_area.width.saturating_sub(SIDE_MARGIN * 2),
        height: list_area.height,
    };
    shelf.prepare(inner.height);

    let width = inner.width as usize;
    // Author and series get fixed shares; the title takes what is left, because
    // it is what the eye looks for first.
    let author_width = (width / 4).clamp(12, 30);
    let series_width = (width / 5).clamp(0, 24);
    let title_width = width.saturating_sub(author_width + series_width + 6);

    let scroll = shelf.scroll();
    let rows: Vec<Line> = shelf
        .entries()
        .iter()
        .enumerate()
        .skip(scroll)
        .take(inner.height as usize)
        .map(|(index, entry)| {
            let selected = index == shelf.cursor();
            let record = &entry.record;

            // A book that has been opened before carries a mark, so picking up
            // where you left off does not need remembering.
            let started = if entry.started { "▌" } else { " " };
            // The series a book belongs to, and where in it, which is what the
            // other orders leave room to show.
            let third = match (&record.series, record.series_index) {
                (Some(name), Some(at)) => format!("{name} {at}"),
                (Some(name), None) => name.clone(),
                _ => String::new(),
            };

            let base = if selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            let dim = if selected {
                base
            } else {
                base.fg(rgb(theme.muted))
            };

            let mut spans = vec![
                Span::styled(started.to_string(), base.fg(rgb(theme.accent))),
                Span::styled(" ", base),
                Span::styled(pad(&record.display_title(), title_width), base),
                Span::styled("  ", base),
                Span::styled(pad(&record.display_authors(), author_width), dim),
            ];
            if series_width > 0 {
                spans.push(Span::styled("  ", base));
                spans.push(Span::styled(pad(&third, series_width), dim));
            }
            Line::from(spans)
        })
        .collect();

    frame.render_widget(Paragraph::new(rows), inner);
    draw_shelf_status(frame, status_area, shelf, theme);

    if shelf.mode == crate::shelf::Mode::Help {
        draw_shelf_help(frame, list_area, theme);
    }
}

fn draw_shelf_status(
    frame: &mut Frame,
    area: Rect,
    shelf: &crate::shelf::Shelf,
    theme: &crate::theme::Theme,
) {
    if let Some(input) = shelf.filter_input() {
        let line = Line::from(vec![
            Span::raw(" ".repeat(SIDE_MARGIN as usize)),
            Span::styled("/", Style::default().fg(rgb(theme.accent))),
            Span::styled(
                input.to_string(),
                Style::default().fg(rgb(theme.foreground)),
            ),
            Span::styled("▏", Style::default().fg(rgb(theme.accent))),
        ]);
        frame.render_widget(Paragraph::new(line), area);
        return;
    }

    let shown = shelf.entries().len();
    let total = shelf.total();
    let left = match shelf.status() {
        Some(message) => message.to_string(),
        None => shelf_summary(shown, total, shelf.filter()),
    };
    let right = i18n::fill("by {}  ·  ? for keys", &[&i18n::t(shelf.order().label())]);
    let gap = area
        .width
        .saturating_sub(cells(&left) as u16 + cells(&right) as u16 + SIDE_MARGIN * 2)
        .max(1);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" ".repeat(SIDE_MARGIN as usize)),
            Span::styled(left, Style::default().fg(rgb(theme.muted))),
            Span::raw(" ".repeat(gap as usize)),
            Span::styled(right, Style::default().fg(rgb(theme.muted))),
        ])),
        area,
    );
}

fn draw_shelf_help(frame: &mut Frame, area: Rect, theme: &crate::theme::Theme) {
    let mut lines = vec![Line::from(Span::styled(
        i18n::t("Library"),
        Style::default()
            .fg(rgb(theme.accent))
            .add_modifier(Modifier::BOLD),
    ))];
    for (keys, what) in crate::shelf::Shelf::bindings() {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {keys:<20}"),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(what, Style::default().fg(rgb(theme.muted))),
        ]));
    }
    let panel = centred(area, 62, lines.len() as u16 + 2);
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

/// The status line when no message is waiting: how many books are shown, out
/// of how many, by which filter.
///
/// The filter is shown as the words they are, the way the command line shows
/// them in `list --filter` — not quoted like a debugger would quote them. The
/// noun agrees with the count it stands beside: one book, two books.
fn shelf_summary(shown: usize, total: usize, filter: &str) -> String {
    if filter.is_empty() {
        i18n::fill(if total == 1 { "{} book" } else { "{} books" }, &[&total])
    } else {
        i18n::fill(
            if total == 1 {
                "{} of {} book  ·  filter {}"
            } else {
                "{} of {} books  ·  filter {}"
            },
            &[&shown, &total, &filter],
        )
    }
}

/// Cuts or pads a value to a fixed width, so the columns line up.
fn pad(text: &str, width: usize) -> String {
    let used = cells(text);
    if used > width {
        return cut(text, width);
    }
    format!("{text}{}", " ".repeat(width - used))
}

// ----- the hit list -----

/// Draws search hits: book, chapter and the passage that matched.
pub fn draw_hits(
    frame: &mut Frame,
    results: &crate::find::Results,
    cursor: usize,
    scroll: usize,
    theme: &crate::theme::Theme,
) {
    let (list_area, status_area) = split_frame(frame.area());
    let inner = Rect {
        x: list_area.x + SIDE_MARGIN,
        y: list_area.y,
        width: list_area.width.saturating_sub(SIDE_MARGIN * 2),
        height: list_area.height,
    };

    // Two rows per hit: where it is, and what it says. One row would force the
    // passage to compete with the title for the same width.
    let mut lines: Vec<Line> = Vec::new();
    for (index, hit) in results.hits.iter().enumerate().skip(scroll) {
        if lines.len() + 3 > inner.height as usize {
            break;
        }
        let selected = index == cursor;
        let heading = if selected {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        };
        lines.push(Line::from(vec![
            Span::styled(
                if selected { "▌ " } else { "  " },
                heading.fg(rgb(theme.accent)),
            ),
            Span::styled(hit.book_title.clone(), heading),
            Span::styled(
                format!("  ·  {}", hit.chapter_title),
                if selected {
                    heading
                } else {
                    Style::default().fg(rgb(theme.muted))
                },
            ),
        ]));
        lines.push(Line::from(Span::styled(
            format!(
                "    {}",
                shorten(&hit.snippet, inner.width.saturating_sub(6) as usize)
            ),
            Style::default().fg(rgb(theme.muted)),
        )));
        lines.push(Line::from(""));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            // The same sentence `find` answers with on the command line.
            i18n::fill("  no book matches {}", &[&results.query]),
            Style::default().fg(rgb(theme.muted)),
        )));
    }
    frame.render_widget(Paragraph::new(lines), inner);

    let source = match results.source {
        crate::find::Source::Index => i18n::t("qmd index"),
        crate::find::Source::Direct => i18n::t("read directly"),
    };
    let hits = results.hits.len();
    let left = i18n::fill(
        if hits == 1 {
            "{} hit for {}  ·  {}"
        } else {
            "{} hits for {}  ·  {}"
        },
        &[&hits, &results.query, &source],
    );
    let right = i18n::t("Enter opens  ·  q leaves").to_string();
    let gap = status_area
        .width
        .saturating_sub(cells(&left) as u16 + cells(&right) as u16 + SIDE_MARGIN * 2)
        .max(1);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" ".repeat(SIDE_MARGIN as usize)),
            Span::styled(left, Style::default().fg(rgb(theme.muted))),
            Span::raw(" ".repeat(gap as usize)),
            Span::styled(right, Style::default().fg(rgb(theme.muted))),
        ])),
        status_area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::epub::Book;
    use crate::identity::BookId;
    use crate::journal::{Journal, State};
    use crate::layout::LayoutOptions;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn a_row_of_chinese_holds_as_many_cells_as_it_looks() {
        // The author column starts where the title column ends, whatever the
        // title is written in: two cells a character, not one.
        assert_eq!(cells("一九七三年的弹子球"), 18);
        assert_eq!(
            cells("：“”…·"),
            6,
            "fullwidth marks are two cells, ambiguous ones one"
        );
        assert_eq!(pad("中文", 6), "中文  ", "four cells of text, two of space");
        assert_eq!(
            cells(&pad("一九七三年的弹子球", 20)),
            20,
            "padded to exactly the column"
        );
        let cut_short = cut("一九七三年的弹子球", 12);
        assert_eq!(cut_short, "一九七三年…", "a character is not cut in half");
        assert!(cells(&cut_short) <= 12, "the mark stays inside the column");
    }

    #[test]
    fn the_shelf_says_how_many_books_there_are_and_what_the_filter_is() {
        // Which of the two keys was asked for, not what it says: this machine
        // may read Chinese, and only the choice of template is under test.
        assert_eq!(shelf_summary(1, 1, ""), i18n::fill("{} book", &[&1]));
        assert_eq!(shelf_summary(0, 0, ""), i18n::fill("{} books", &[&0]));
        assert_eq!(shelf_summary(7, 7, ""), i18n::fill("{} books", &[&7]));

        // The same words `omaread list --filter 村上` prints, and not a debug
        // rendering of them in quotes.
        let line = shelf_summary(1, 13, "村上");
        assert_eq!(
            line,
            i18n::fill("{} of {} books  ·  filter {}", &[&1, &13, &"村上"])
        );
        assert!(!line.contains('"'), "{line}");
        assert_eq!(
            shelf_summary(1, 1, "one"),
            i18n::fill("{} of {} book  ·  filter {}", &[&1, &1, &"one"])
        );
    }

    /// A book whose one chapter holds one flow picture: 80x32 pixels, which is
    /// 10 cells wide and two down at an 8x16 cell — a mark, narrower than any
    /// room the test gives it, and in the flow of text rather than a cover, so
    /// it keeps that size.
    fn picture_book(path: &std::path::Path) {
        use std::io::Write;
        let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            80,
            32,
            image::Rgba([200, 100, 50, 255]),
        ));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        let png = bytes.into_inner();

        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
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
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Centre</dc:title></metadata>
<manifest>
  <item id="c0" href="ch0.xhtml" media-type="application/xhtml+xml"/>
  <item id="art" href="art.png" media-type="image/png"/>
</manifest>
<spine><itemref idref="c0"/></spine></package>"#,
        );
        write(
            "OEBPS/ch0.xhtml",
            br#"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>t</title></head>
<body><img src="art.png" alt="a diagram"/></body></html>"#,
        );
        write("OEBPS/art.png", &png);
        zip.finish().unwrap();
    }

    #[test]
    fn a_picture_is_centred_in_the_column_whichever_way_it_is_drawn() {
        // A picture is a figure on its own lines, so it stands in the middle
        // of the column rather than at its left edge: 80x32 pixels is 10
        // cells at an 8x16 cell, and 10 cells in the 36-cell room of a
        // 40-cell frame start 13 cells in. Both ways of drawing have to
        // agree on that column — the block fallback pads each row out to it,
        // and a pixel escape is emitted there — or one backend would draw
        // the same book differently from another.
        let book = std::env::temp_dir().join("omaread-ui-centre.epub");
        picture_book(&book);
        for backend in [
            crate::image::Backend::HalfBlocks,
            crate::image::Backend::Kitty,
            crate::image::Backend::Sixel,
        ] {
            let dir = std::env::temp_dir().join(format!("omaread-ui-centre-journal-{backend:?}"));
            std::fs::remove_dir_all(&dir).ok();
            let mut app = App::new(
                Book::open(&book).unwrap(),
                BookId::of_file(&book).unwrap(),
                Journal::open(&dir).unwrap(),
                &State::default(),
                LayoutOptions {
                    max_width: u16::MAX,
                },
            )
            .unwrap();
            app.set_image_backend(
                backend,
                crate::image::CellSize {
                    width: 8,
                    height: 16,
                },
            );

            let mut terminal = Terminal::new(TestBackend::new(40, 24)).unwrap();
            let mut placed = Vec::new();
            terminal
                .draw(|frame| placed = draw(frame, &mut app))
                .unwrap();

            let picture: Vec<&SourceLine> = app
                .visible_lines()
                .iter()
                .filter(|line| matches!(line.kind, LineKind::Image { .. }))
                .collect();
            assert_eq!(picture.len(), 2, "{backend:?}: the picture is two rows");
            assert!(
                picture.iter().all(|line| line.indent == 13),
                "{backend:?}: 10 cells in a 36-cell room start 13 cells in"
            );

            // The column the picture actually starts at: the text area sits
            // two cells in from the frame, so the offset lands at 15.
            let buffer = terminal.backend().buffer();
            let drawn_at = |y: u16| {
                (0..40u16).find(|x| buffer.cell((*x, y)).map(|cell| cell.symbol()) == Some("▀"))
            };
            match backend {
                // The block fallback writes cells, so the offset is blank
                // cells in front of every row of the picture.
                crate::image::Backend::HalfBlocks => {
                    assert_eq!(drawn_at(0), Some(15), "the cells start at the offset");
                    assert!(placed.is_empty(), "a block picture places no escape");
                }
                // A pixel protocol paints past the buffer, so the escape is
                // emitted at the offset column instead.
                _ => {
                    assert_eq!(drawn_at(0), None, "pixels are not cells");
                    assert_eq!(placed.len(), 1, "{backend:?}: one escape, one picture");
                    assert_eq!(
                        placed[0].column, 15,
                        "{backend:?}: the escape moves to the offset column first"
                    );
                    assert_eq!(placed[0].row, 0, "the picture starts on its own row");
                }
            }
            std::fs::remove_dir_all(&dir).ok();
        }
        std::fs::remove_file(book).ok();
    }
}
