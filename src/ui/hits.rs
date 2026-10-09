//! The hit list: where each match was found and what it says, and the status
//! row that names the query and how many books answered it.
//!
//! Two rows a hit — the place and the passage — because one row would make
//! the passage compete with the title for the same width.

use super::{page_areas, rgb, status_row};

use crate::i18n;
use crate::measure::shorten;

use ratatui::Frame;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// Draws search hits: book, chapter and the passage that matched.
pub fn draw_hits(
    frame: &mut Frame,
    results: &crate::find::Results,
    cursor: usize,
    scroll: usize,
    theme: &crate::theme::Theme,
) {
    let Some((_, inner, status_area)) = page_areas(frame, theme) else {
        return;
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
    if status_area.height == 0 {
        return;
    }

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
    let right = i18n::t("Enter opens  ·  q leaves");
    frame.render_widget(
        Paragraph::new(status_row(
            status_area.width,
            theme,
            left,
            right,
            Style::default().fg(rgb(theme.muted)),
        )),
        status_area,
    );
}
