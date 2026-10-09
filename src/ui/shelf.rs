//! The library view: the authors, then the books of one of them, the columns
//! a shelf has room for, and the row underneath that says what is being shown.
//!
//! The shelf is the reader's front door, so it draws itself: the same frame
//! split, the same too-small face and the same status row as the page, read
//! from the view they belong to.

use super::{
    SIDE_MARGIN, binding_line, help_panel, page_areas, prompt_line, rgb, status_row,
};

use crate::i18n;
use crate::library::{Author, Entry};
use crate::measure::{cells, fit, pad};
use crate::shelf::Level;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// Below this width a row inside an author shows only the title: a series
/// name and a title beside it need room for both, and the title is what is
/// being looked for.
const SERIES_COLUMN_MIN: usize = 44;

/// Below this width the shelf's status line keeps only the count.
/// "authors · ? for keys" is the least of what it says; the count is the most.
const SHELF_HINTS_MIN: usize = 34;

/// Draws the shelf: the authors with their counts, or the books of one author,
/// with a filter prompt when one is being typed.
pub fn draw_shelf(frame: &mut Frame, shelf: &mut crate::shelf::Shelf, theme: &crate::theme::Theme) {
    let Some((list_area, inner, status_area)) = page_areas(frame, theme) else {
        return;
    };
    shelf.prepare(inner.height);
    shelf.set_rows_area(inner.x, inner.y, inner.width, inner.height);

    let width = inner.width as usize;
    let scroll = shelf.scroll();
    let rows: Vec<Line> = match shelf.level() {
        Level::Authors => shelf
            .author_rows()
            .skip(scroll)
            .take(inner.height as usize)
            .map(|(row, author)| author_row(author, row == shelf.cursor(), width, theme))
            .collect(),
        Level::Books => {
            // One author is the whole list, so the author column is gone and
            // the title keeps the room it had: the series takes a share of
            // what is left and steps aside alone when the shelf is too narrow
            // for both, the way title and author used to.
            let series_width = if width >= SERIES_COLUMN_MIN {
                (width / 5).clamp(0, 24)
            } else {
                0
            };
            let title_width = width.saturating_sub(series_width + 2);
            shelf
                .rows()
                .skip(scroll)
                .take(inner.height as usize)
                .map(|(row, entry)| book_row(entry, row == shelf.cursor(), title_width, series_width, theme))
                .collect()
        }
    };

    frame.render_widget(Paragraph::new(rows), inner);
    if status_area.height > 0 {
        draw_shelf_status(frame, status_area, shelf, theme);
    }

    if shelf.mode == crate::shelf::Mode::Help {
        draw_shelf_help(frame, list_area, theme);
    }
}

/// One row of the authors: the name on the left, and against the right edge
/// how many books it names — a name alone says nothing about how much of the
/// shelf is waiting behind it.
fn author_row(author: &Author, selected: bool, width: usize, theme: &crate::theme::Theme) -> Line<'static> {
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
    // The group that names nobody is named the same way the rest of the
    // reader names it, in the language this machine reads.
    let name = author.name.as_deref().unwrap_or(i18n::t("no author"));
    let count = author.books.len().to_string();
    let name_width = width.saturating_sub(cells(&count) + 1);
    Line::from(vec![
        Span::styled(pad(name, name_width), dim),
        Span::styled(" ", dim),
        Span::styled(count, dim),
    ])
}

/// One row of an author's books: the title, and the series it belongs to when
/// there is room to say so.
fn book_row(
    entry: &Entry,
    selected: bool,
    title_width: usize,
    series_width: usize,
    theme: &crate::theme::Theme,
) -> Line<'static> {
    let record = &entry.record;
    let third = record.series_label().unwrap_or_default();

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

    let mut spans = vec![Span::styled(pad(&record.display_title(), title_width), base)];
    if series_width > 0 {
        spans.push(Span::styled("  ", base));
        spans.push(Span::styled(pad(&third, series_width), dim));
    }
    Line::from(spans)
}

fn draw_shelf_status(
    frame: &mut Frame,
    area: Rect,
    shelf: &crate::shelf::Shelf,
    theme: &crate::theme::Theme,
) {
    if let Some(input) = shelf.filter_input() {
        frame.render_widget(Paragraph::new(prompt_line(input, theme)), area);
        return;
    }

    // Which list is showing decides which rows are counted: the authors, or
    // the books of the author whose name the summary carries.
    let (shown, author) = match shelf.level() {
        Level::Authors => (shelf.author_rows().len(), None),
        Level::Books => (shelf.rows().len(), shelf.author_name()),
    };
    let full = match shelf.status() {
        Some(message) => message.to_string(),
        None => shelf_summary(
            shelf.level(),
            author,
            shown,
            shelf.total(),
            shelf.filter(),
        ),
    };
    // The count is the one thing the row must say, so on a narrow shelf the
    // hint gives way, and the count is cut rather than either pushing the
    // other off the row.
    let right = if area.width as usize >= SHELF_HINTS_MIN {
        i18n::fill("{}  ·  ? for keys", &[&i18n::t(shelf.level().label())])
    } else {
        String::new()
    };
    let room = (area.width as usize).saturating_sub(SIDE_MARGIN as usize * 2 + cells(&right) + 1);
    let left = fit(&full, room);

    frame.render_widget(
        Paragraph::new(status_row(
            area.width,
            theme,
            left,
            right,
            Style::default().fg(rgb(theme.muted)),
        )),
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
        lines.push(binding_line(keys, what, theme));
    }
    help_panel(frame, area, 62, lines, theme);
}

/// The status line when no message is waiting: which list is showing, how much
/// of it is left after the filter, and which author the books belong to.
///
/// The filter is shown as the words they are, the way the command line shows
/// them in `list --filter` — not quoted like a debugger would quote them. The
/// noun agrees with the count it stands beside: one author, two books.
fn shelf_summary(
    level: Level,
    author: Option<&str>,
    shown: usize,
    total: usize,
    filter: &str,
) -> String {
    match level {
        Level::Authors => {
            if filter.is_empty() {
                i18n::fill(
                    if total == 1 { "{} author" } else { "{} authors" },
                    &[&total],
                )
            } else {
                i18n::fill(
                    if total == 1 {
                        "{} of {} author  ·  filter {}"
                    } else {
                        "{} of {} authors  ·  filter {}"
                    },
                    &[&shown, &total, &filter],
                )
            }
        }
        Level::Books => {
            // The group that names nobody is named here, so the line always
            // says whose books these are.
            let author = author.unwrap_or(i18n::t("no author"));
            if filter.is_empty() {
                i18n::fill(
                    if shown == 1 {
                        "{}  ·  {} book"
                    } else {
                        "{}  ·  {} books"
                    },
                    &[&author, &shown],
                )
            } else {
                i18n::fill(
                    if total == 1 {
                        "{}  ·  {} of {} book  ·  filter {}"
                    } else {
                        "{}  ·  {} of {} books  ·  filter {}"
                    },
                    &[&author, &shown, &total, &filter],
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::State;
    use crate::shelf::Mode;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// A library of `(title, author, series)` books: an empty author is a
    /// book that names nobody.
    fn state_of(books: &[(&str, &str, &str)]) -> State {
        let mut state = State::default();
        for (index, (title, author, series)) in books.iter().enumerate() {
            state.apply(crate::journal::Event {
                at: chrono::Utc::now(),
                book: format!("sha256:{}", format!("{:02x}", index + 1).repeat(32)),
                payload: crate::journal::Payload::BookSeen {
                    title: Some((*title).to_string()),
                    authors: if author.is_empty() {
                        Vec::new()
                    } else {
                        vec![(*author).to_string()]
                    },
                    path: std::path::PathBuf::from(format!("/books/{index}.epub")),
                },
            });
            if !series.is_empty() {
                state.apply(crate::journal::Event {
                    at: chrono::Utc::now(),
                    book: format!("sha256:{}", format!("{:02x}", index + 1).repeat(32)),
                    payload: crate::journal::Payload::MetadataSet {
                        title: None,
                        authors: None,
                        series: Some((*series).to_string()),
                        series_index: None,
                        tags: None,
                        rating: None,
                        publisher: None,
                        year: None,
                        language: None,
                    },
                });
            }
        }
        state
    }

    /// Any shelf drawn into a test terminal, one line to a string.
    fn shelf_text(shelf: &mut crate::shelf::Shelf, width: u16, height: u16) -> String {
        let theme = crate::theme::Theme::default();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw_shelf(frame, shelf, &theme))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The same, from a fresh shelf of books, on the level asked for.
    fn shelf_text_of(books: &[(&str, &str, &str)], width: u16, height: u16, level: Level) -> String {
        let mut shelf = crate::shelf::Shelf::new(&state_of(books));
        if level == Level::Books && !books.is_empty() {
            shelf.handle_key(crate::testkit::key(
                ratatui::crossterm::event::KeyCode::Enter,
            ));
        }
        shelf_text(&mut shelf, width, height)
    }

    /// The same, for the empty shelf, where the count is all there is to see.
    fn shelf_text_blank(width: u16, height: u16) -> String {
        shelf_text_of(&[], width, height, Level::Authors)
    }

    /// A line of Chinese comes out of the buffer with a space where the
    /// second cell of a wide glyph was, so matching ignores whitespace — the
    /// words are what is under test, and they keep their own spacing.
    fn squeezed(text: &str) -> String {
        text.chars().filter(|c| !c.is_whitespace()).collect()
    }

    /// The row a name sits on, as the terminal drew it.
    fn row_containing(text: &str, needle: &str) -> String {
        let needle = squeezed(needle);
        text.lines()
            .find(|line| squeezed(line).contains(&needle))
            .unwrap_or_else(|| panic!("{needle:?} is not on the shelf: {text:?}"))
            .to_string()
    }

    #[test]
    fn only_a_single_cell_gets_the_face() {
        let text = shelf_text_blank(2, 1);
        assert!(text.contains(":("), "{text:?}");
        let room = shelf_text_blank(4, 2);
        assert!(!room.contains(":("), "two rows are a page: {room:?}");
    }

    #[test]
    fn a_small_frame_gives_its_only_rows_to_the_page() {
        // The status row is the first thing to go, so a shelf too short for
        // both the count and the books still shows the books — which is only
        // proven with a book on the shelf.
        let books: &[(&str, &str, &str)] = &[("A Book", "An Author", "")];
        let tall = shelf_text_of(books, 60, 6, Level::Books);
        assert!(tall.contains('1'), "the count is there: {tall:?}");
        let short = shelf_text_of(books, 60, 3, Level::Books);
        assert!(short.contains("A Book"), "the book is shown: {short:?}");
        assert!(
            !short.contains('1'),
            "the count gave its row to the page: {short:?}"
        );
    }

    #[test]
    fn a_narrow_shelf_keeps_the_count_but_drops_the_hint() {
        // The hint writes the one mark the count never does, so it is what
        // tells the two rows apart whatever language this machine reads.
        let wide = shelf_text_blank(60, 6);
        assert!(wide.contains('?'), "{wide:?}");
        assert!(wide.contains('0'), "{wide:?}");

        let narrow = shelf_text_blank(30, 6);
        assert!(narrow.contains('0'), "the count stays: {narrow:?}");
        assert!(!narrow.contains('?'), "the hint goes: {narrow:?}");
    }

    #[test]
    fn an_author_row_names_the_writer_and_counts_the_books_on_the_right() {
        let text = shelf_text_of(
            &[("Zebra", "Zena", ""), ("Apple", "Adam", ""), ("Mango", "Adam", "")],
            60,
            6,
            Level::Authors,
        );
        let row = row_containing(&text, "Adam");
        assert!(
            row.trim_start().starts_with("Adam"),
            "the name is on the left: {row:?}"
        );
        assert!(
            row.trim_end().ends_with('2'),
            "both of his books counted on the right: {row:?}"
        );
        let zena = row_containing(&text, "Zena");
        assert!(zena.trim_end().ends_with('1'), "{zena:?}");

        // A book that names nobody has a row of its own, in this machine's
        // words rather than a dash.
        let text = shelf_text_of(&[("Orphan", "", "")], 60, 6, Level::Authors);
        let row = row_containing(&text, i18n::t("no author"));
        assert!(row.trim_end().ends_with('1'), "{row:?}");
    }

    #[test]
    fn a_books_row_shows_the_title_and_series_and_leaves_the_author_out() {
        // Inside one author the name is in every row and in the status line:
        // a column of it would say the same thing four times.
        let text = shelf_text_of(
            &[("The Hobbit", "Tolkien", "Middle-earth")],
            100,
            6,
            Level::Books,
        );
        let row = row_containing(&text, "The Hobbit");
        assert!(row.contains("Middle-earth"), "the series still shows: {row:?}");
        assert!(!row.contains("Tolkien"), "the author does not: {row:?}");

        // The status line is where the author's name and the count live —
        // and the noun beside a count of one is singular.
        let status = row_containing(&text, &i18n::fill("{}  ·  {} book", &[&"Tolkien", &1]));
        assert!(!status.is_empty(), "the status names the author: {text:?}");
    }

    #[test]
    fn the_shelf_says_which_list_it_counts_and_what_the_filter_left() {
        // Which of the keys was asked for, not what it says: this machine may
        // read Chinese, and only the choice of template is under test.
        assert_eq!(
            shelf_summary(Level::Authors, None, 5, 5, ""),
            i18n::fill("{} authors", &[&5])
        );
        assert_eq!(
            shelf_summary(Level::Authors, None, 1, 1, ""),
            i18n::fill("{} author", &[&1])
        );
        assert_eq!(
            shelf_summary(Level::Authors, None, 1, 5, "村上"),
            i18n::fill("{} of {} authors  ·  filter {}", &[&1, &5, &"村上"])
        );

        // The books are always somebody's, so the line carries the name.
        assert_eq!(
            shelf_summary(Level::Books, Some("村上春树"), 2, 2, ""),
            i18n::fill("{}  ·  {} books", &[&"村上春树", &2])
        );
        assert_eq!(
            shelf_summary(Level::Books, Some("村上春树"), 1, 2, "norwegian"),
            i18n::fill(
                "{}  ·  {} of {} books  ·  filter {}",
                &[&"村上春树", &1, &2, &"norwegian"]
            )
        );
        // The group that names nobody is still named, in this machine's words.
        assert_eq!(
            shelf_summary(Level::Books, None, 1, 1, ""),
            i18n::fill("{}  ·  {} book", &[&i18n::t("no author"), &1])
        );
        // Not a debug rendering of any of it, in quotes.
        assert!(!shelf_summary(Level::Authors, None, 1, 1, "x").contains('"'));
    }

    #[test]
    fn the_help_lists_the_keys_of_both_levels_and_no_sort_key() {
        let mut shelf = crate::shelf::Shelf::new(&state_of(&[("Anathem", "Stephenson", "")]));
        shelf.mode = Mode::Help;
        let text = shelf_text(&mut shelf, 90, 24);
        assert!(text.contains("Enter l"), "entering is listed: {text:?}");
        assert!(text.contains("Esc"), "the way back is listed: {text:?}");

        // The order is not a choice any more, so no key changes it and no
        // line offers to.
        let bindings = crate::shelf::Shelf::bindings();
        assert!(
            !bindings
                .iter()
                .any(|(keys, _)| keys.split_whitespace().any(|word| word == "s")),
            "the sort key is gone: {bindings:?}"
        );
        assert!(
            !bindings.iter().any(|(_, what)| what.contains("cycle")),
            "and nothing promises to reorder the shelf: {bindings:?}"
        );
        assert!(!text.contains("sorted by"), "{text:?}");
    }
}
