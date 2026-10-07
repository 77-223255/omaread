//! Wraps blocks into terminal lines.
//!
//! Every produced line records where it came from: the block index and the
//! character offset within that block. That mapping is what lets a reading
//! position survive a resize.

use crate::doc::{Block, BlockKind, Chapter, Run, RunStyle};
use unicode_width::UnicodeWidthChar;

/// A styled piece of a laid-out line.
#[derive(Debug, Clone)]
pub struct Piece {
    pub text: String,
    pub style: RunStyle,
    /// True for text the layout added, such as a list marker or an image label.
    /// Decoration carries no block offsets, so the cursor skips it.
    pub decoration: bool,
}

impl Piece {
    fn text(text: String, style: RunStyle) -> Self {
        Self {
            text,
            style,
            decoration: false,
        }
    }

    fn decoration(text: String) -> Self {
        Self {
            text,
            style: RunStyle::default(),
            decoration: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Heading(u8),
    Body,
    Quote,
    Code,
    Rule,
    /// One row of a picture. `row` says which, so the view knows what to paint.
    Image {
        row: u16,
        rows: u16,
    },
    /// Vertical spacing between blocks.
    Blank,
}

/// A picture that is ready to be placed, with the box it needs.
#[derive(Debug, Clone)]
pub struct ImagePlacement {
    pub block: usize,
    pub rows: u16,
    /// Cells across, as `image::measure` counted them for the same room. The
    /// centre offset is worked out from this, so the indent and the drawn
    /// picture cannot disagree about how wide it is.
    pub cols: u16,
}

/// How far a picture is pushed in from the left of the room, so a picture
/// narrower than the column stands centred on it — a picture is a figure
/// drawn on its own lines, and a figure is not glued to the margin.
///
/// Floor division: one cell of leftover slack goes to the left. A picture
/// wider than the room is not reachable — `image::measure` caps the picture
/// at the room — and that cap is asserted here rather than assumed, so a
/// change to it fails as an assertion instead of as a picture drawn off the
/// left edge.
pub fn centred_offset(room: u16, picture: u16) -> u16 {
    assert!(
        picture <= room,
        "a picture was placed wider than the room it is drawn in"
    );
    (room - picture) / 2
}

#[derive(Debug, Clone)]
pub struct Line {
    /// Index of the source block in the chapter.
    pub block: usize,
    /// Character offset into the block's plain text where this line starts.
    pub offset: usize,
    pub indent: u16,
    pub kind: LineKind,
    pub pieces: Vec<Piece>,
}

impl Line {
    fn blank(block: usize) -> Self {
        Self {
            block,
            offset: 0,
            indent: 0,
            kind: LineKind::Blank,
            pieces: Vec::new(),
        }
    }

    /// Number of characters of block text this line shows. Decoration, rules and
    /// pictures carry no block text, so the cursor cannot stand on them.
    pub fn text_len(&self) -> usize {
        if matches!(
            self.kind,
            LineKind::Blank | LineKind::Rule | LineKind::Image { .. }
        ) {
            return 0;
        }
        self.pieces
            .iter()
            .filter(|p| !p.decoration)
            .map(|p| p.text.chars().count())
            .sum()
    }

    /// True when this line can hold the cursor.
    pub fn is_selectable(&self) -> bool {
        self.text_len() > 0
    }
}

/// Where a text position sits on screen, and where a screen position sits in the
/// text. The layout is the only place that knows both.
pub struct Index<'a> {
    lines: &'a [Line],
}

impl<'a> Index<'a> {
    pub fn new(lines: &'a [Line]) -> Self {
        Self { lines }
    }

    /// The line holding a block offset, or the closest line before it.
    pub fn line_of(&self, block: usize, offset: usize) -> Option<usize> {
        let mut best = None;
        for (index, line) in self.lines.iter().enumerate() {
            if !line.is_selectable() {
                continue;
            }
            if line.block < block || (line.block == block && line.offset <= offset) {
                best = Some(index);
            } else {
                break;
            }
        }
        best.or_else(|| self.lines.iter().position(|l| l.is_selectable()))
    }

    /// The first selectable line at or after `from`.
    pub fn next_selectable(&self, from: usize) -> Option<usize> {
        self.lines
            .iter()
            .enumerate()
            .skip(from)
            .find(|(_, l)| l.is_selectable())
            .map(|(i, _)| i)
    }

    /// The last selectable line at or before `from`.
    pub fn previous_selectable(&self, from: usize) -> Option<usize> {
        self.lines
            .iter()
            .enumerate()
            .take(from + 1)
            .rfind(|(_, l)| l.is_selectable())
            .map(|(i, _)| i)
    }
}

/// Reading width and margins.
#[derive(Debug, Clone, Copy)]
pub struct LayoutOptions {
    /// Maximum text width in columns, before indentation.
    pub max_width: u16,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        // Use the full window. A narrower measure, around 66 columns, reads
        // better for body text; `max_width` in the configuration sets one.
        Self {
            max_width: u16::MAX,
        }
    }
}

/// Lays out a chapter with room reserved for pictures.
pub fn layout_full(
    chapter: &Chapter,
    available_width: u16,
    options: LayoutOptions,
    images: &[ImagePlacement],
) -> Vec<Line> {
    // The reading width never exceeds the window. A configured maximum below
    // the window width is intentional, so it is not raised.
    let width = available_width.min(options.max_width).max(1);
    let mut lines: Vec<Line> = Vec::new();

    for (index, block) in chapter.blocks.iter().enumerate() {
        let indent = indent_for(&block.kind);
        let usable = width.saturating_sub(indent).max(1);

        if needs_leading_blank(&block.kind, lines.last().map(|l| l.kind)) {
            lines.push(Line::blank(index));
        }

        match &block.kind {
            BlockKind::Rule => lines.push(Line {
                block: index,
                offset: 0,
                indent,
                kind: LineKind::Rule,
                pieces: Vec::new(),
            }),
            // A code line keeps its own line breaks, but a line longer than the
            // window has to wrap: cutting it off would hide the code.
            BlockKind::Code => wrap_code(index, block, usable, indent, &mut lines),
            BlockKind::Image { .. } => match images.iter().find(|i| i.block == index) {
                // A picture that could be read occupies as many lines as it is
                // tall; the view paints one row into each. Its indent is the
                // centre offset, not a block indent — a picture has no block
                // indent — and that one number is what both ways of drawing,
                // cells and pixel escapes, place by.
                Some(placement) => {
                    let indent = centred_offset(width, placement.cols);
                    for row in 0..placement.rows {
                        lines.push(Line {
                            block: index,
                            offset: 0,
                            indent,
                            kind: LineKind::Image {
                                row,
                                rows: placement.rows,
                            },
                            pieces: Vec::new(),
                        });
                    }
                }
                // Without pixels, the alt text has to do.
                None => lines.push(Line {
                    block: index,
                    offset: 0,
                    indent,
                    kind: LineKind::Image { row: 0, rows: 1 },
                    pieces: vec![Piece::decoration(format!("[{}]", block.plain_text()))],
                }),
            },
            kind => {
                let line_kind = match kind {
                    BlockKind::Heading(level) => LineKind::Heading(*level),
                    BlockKind::Quote => LineKind::Quote,
                    _ => LineKind::Body,
                };
                let marker = list_marker(kind);
                wrap_block(index, block, usable, indent, line_kind, marker, &mut lines);
            }
        }
    }

    // Trailing spacing serves no purpose.
    while matches!(lines.last().map(|l| l.kind), Some(LineKind::Blank)) {
        lines.pop();
    }
    lines
}

/// Lays out one line of code, breaking it only where it exceeds the window.
///
/// The break happens at the last space that fits, as in prose, but a wrapped
/// remainder is marked and indented so it cannot be mistaken for a line of its
/// own.
fn wrap_code(block: usize, source: &Block, usable: u16, indent: u16, out: &mut Vec<Line>) {
    const CONTINUATION: &str = "… ";

    let text = source.plain_text();
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        out.push(Line {
            block,
            offset: 0,
            indent,
            kind: LineKind::Code,
            pieces: Vec::new(),
        });
        return;
    }

    let mut at = 0usize;
    let mut first = true;
    while at < chars.len() {
        let budget = if first {
            usable
        } else {
            usable.saturating_sub(CONTINUATION.chars().count() as u16)
        }
        .max(1);
        let end = break_at(&chars, at, budget);
        let mut pieces = Vec::new();
        if !first {
            pieces.push(Piece::decoration(CONTINUATION.to_string()));
        }
        pieces.push(Piece::text(
            chars[at..end].iter().collect(),
            RunStyle {
                code: true,
                ..RunStyle::default()
            },
        ));
        out.push(Line {
            block,
            offset: at,
            indent,
            kind: LineKind::Code,
            pieces,
        });
        // A break inside code must not swallow spaces: they are indentation.
        at = end;
        first = false;
    }
}

fn indent_for(kind: &BlockKind) -> u16 {
    match kind {
        BlockKind::Quote => 4,
        BlockKind::Code => 4,
        BlockKind::ListItem { depth, .. } => 2 + (*depth as u16) * 2,
        _ => 0,
    }
}

fn list_marker(kind: &BlockKind) -> Option<String> {
    match kind {
        BlockKind::ListItem { ordinal, .. } => Some(match ordinal {
            Some(n) => format!("{n}. "),
            None => "- ".to_string(),
        }),
        _ => None,
    }
}

/// A blank line before every block but the first, with two exceptions: list
/// items run together after body text, and the lines of one code block stay
/// together.
fn needs_leading_blank(kind: &BlockKind, previous: Option<LineKind>) -> bool {
    let Some(previous) = previous else {
        return false;
    };
    if previous == LineKind::Blank {
        return false;
    }
    match kind {
        BlockKind::ListItem { .. } => !matches!(previous, LineKind::Body),
        BlockKind::Code => previous != LineKind::Code,
        _ => true,
    }
}

/// Style spans over the block's plain text, as character ranges.
struct StyleMap {
    spans: Vec<(usize, usize, RunStyle)>,
}

impl StyleMap {
    fn build(runs: &[Run]) -> Self {
        let mut spans = Vec::with_capacity(runs.len());
        let mut at = 0usize;
        for run in runs {
            let len = run.text.chars().count();
            spans.push((at, at + len, run.style));
            at += len;
        }
        Self { spans }
    }

    /// Splits a character range into pieces of uniform style.
    fn pieces(&self, chars: &[char], start: usize, end: usize) -> Vec<Piece> {
        let mut pieces: Vec<Piece> = Vec::new();
        for (span_start, span_end, style) in &self.spans {
            let from = (*span_start).max(start);
            let to = (*span_end).min(end);
            if from >= to {
                continue;
            }
            let text: String = chars[from..to].iter().collect();
            match pieces.last_mut() {
                Some(last) if last.style == *style => last.text.push_str(&text),
                _ => pieces.push(Piece::text(text, *style)),
            }
        }
        pieces
    }
}

fn wrap_block(
    index: usize,
    block: &Block,
    usable: u16,
    indent: u16,
    kind: LineKind,
    marker: Option<String>,
    out: &mut Vec<Line>,
) {
    let text = block.plain_text();
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return;
    }
    let styles = StyleMap::build(&block.runs);

    // A list marker shortens every line: the first carries it, the rest are
    // indented by its width so the text stays aligned.
    let marker_width = marker.as_ref().map(|m| display_width(m)).unwrap_or(0) as u16;
    let budget = usable.saturating_sub(marker_width).max(1);
    let mut first = true;
    let mut at = 0usize;

    while at < chars.len() {
        let end = break_at(&chars, at, budget);
        let mut pieces = Vec::new();
        if first
            && let Some(marker) = &marker {
                pieces.push(Piece::decoration(marker.clone()));
            }
        pieces.extend(styles.pieces(&chars, at, end));

        out.push(Line {
            block: index,
            offset: at,
            indent: if first { indent } else { indent + marker_width },
            kind,
            pieces,
        });

        at = skip_spaces(&chars, end);
        first = false;
    }
}

/// Finds where to break a line, preferring the last space that fits.
fn break_at(chars: &[char], start: usize, budget: u16) -> usize {
    let budget = budget as usize;
    let mut width = 0usize;
    let mut last_space: Option<usize> = None;
    let mut at = start;

    while at < chars.len() {
        let ch = chars[at];
        let w = ch.width().unwrap_or(0);
        if width + w > budget {
            // A space that overflows is dropped by the break anyway, so the
            // line ends right before it and the text still fills the budget.
            if ch == ' ' {
                return at;
            }
            // Break at the last space, unless a single word exceeds the budget.
            return match last_space {
                Some(space) if space > start => space,
                _ => at.max(start + 1),
            };
        }
        if ch == ' ' {
            last_space = Some(at);
        }
        width += w;
        at += 1;
    }
    chars.len()
}

fn skip_spaces(chars: &[char], mut at: usize) -> usize {
    while at < chars.len() && chars[at] == ' ' {
        at += 1;
    }
    at
}

fn display_width(text: &str) -> usize {
    text.chars().map(|c| c.width().unwrap_or(0)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::Run;

    fn chapter_of(blocks: Vec<Block>) -> Chapter {
        Chapter {
            href: "ch.xhtml".into(),
            blocks,
            ..Chapter::default()
        }
    }

    fn paragraph(text: &str) -> Block {
        Block {
            kind: BlockKind::Paragraph,
            runs: vec![Run {
                text: text.into(),
                style: RunStyle::default(),
            }],
        }
    }

    fn picture() -> Block {
        Block {
            kind: BlockKind::Image {
                src: Some("art.png".into()),
            },
            runs: vec![Run {
                text: "a diagram".into(),
                style: RunStyle::default(),
            }],
        }
    }

    /// The image lines of a layout, in order.
    fn pictures(lines: &[Line]) -> Vec<&Line> {
        lines
            .iter()
            .filter(|line| matches!(line.kind, LineKind::Image { .. }))
            .collect()
    }

    #[test]
    fn a_picture_is_centred_in_the_room_it_is_drawn_in() {
        // A picture is a figure on its own lines, so it stands in the middle
        // of the column rather than glued to the margin: 6 cells in an
        // 11-cell room start two cells in, the odd cell of slack going left.
        let chapter = chapter_of(vec![picture()]);
        let lines = layout_full(
            &chapter,
            11,
            LayoutOptions { max_width: 11 },
            &[ImagePlacement {
                block: 0,
                rows: 3,
                cols: 6,
            }],
        );
        let drawn = pictures(&lines);
        assert_eq!(drawn.len(), 3, "the reserved lines are the measured rows");
        assert!(
            drawn.iter().all(|line| line.indent == 2),
            "every row of the picture starts two cells in"
        );

        // No slack to centre in: a cover or a page that fills the column
        // starts where it always did, at the left edge of the room.
        let lines = layout_full(
            &chapter,
            11,
            LayoutOptions { max_width: 11 },
            &[ImagePlacement {
                block: 0,
                rows: 2,
                cols: 11,
            }],
        );
        let drawn = pictures(&lines);
        assert_eq!(drawn.len(), 2, "the reserved lines are the measured rows");
        assert!(
            drawn.iter().all(|line| line.indent == 0),
            "a picture filling the column has no offset"
        );
    }

    #[test]
    fn wraps_at_word_boundaries_and_records_where_each_line_starts() {
        let chapter = chapter_of(vec![paragraph("aaa bbb ccc ddd")]);
        let lines = layout_full(&chapter, 100, LayoutOptions { max_width: 11 }, &[]);
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.pieces.iter().map(|p| p.text.as_str()).collect())
            .collect();
        assert_eq!(texts, vec!["aaa bbb ccc", "ddd"]);
        assert_eq!(lines[0].offset, 0);
        // "ddd" starts after "aaa bbb ccc ".
        assert_eq!(lines[1].offset, 12);
        assert!(lines.iter().all(|l| l.block == 0));
    }

    #[test]
    fn breaks_words_longer_than_the_line() {
        let chapter = chapter_of(vec![paragraph("abcdefghij")]);
        let lines = layout_full(&chapter, 100, LayoutOptions { max_width: 24 }, &[]);
        // Minimum usable width is 20, so this fits on one line.
        assert_eq!(lines.len(), 1);

        let chapter = chapter_of(vec![paragraph(&"x".repeat(50))]);
        let lines = layout_full(&chapter, 100, LayoutOptions { max_width: 20 }, &[]);
        assert!(lines.len() > 1);
        assert!(lines.iter().all(|l| !l.pieces.is_empty()));
    }

    #[test]
    fn separates_paragraphs_with_a_blank_line() {
        let chapter = chapter_of(vec![paragraph("one"), paragraph("two")]);
        let lines = layout_full(&chapter, 100, LayoutOptions::default(), &[]);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[1].kind, LineKind::Blank);
    }

    #[test]
    fn keeps_styles_across_a_line_break() {
        let block = Block {
            kind: BlockKind::Paragraph,
            runs: vec![
                Run {
                    text: "plain ".into(),
                    style: RunStyle::default(),
                },
                Run {
                    text: "bold text here".into(),
                    style: RunStyle {
                        bold: true,
                        ..RunStyle::default()
                    },
                },
            ],
        };
        let lines = layout_full(
            &chapter_of(vec![block]),
            100,
            LayoutOptions { max_width: 12 },
            &[],
        );
        assert!(lines.len() > 1);
        // The bold run survives the break on both lines.
        assert!(lines[0].pieces.iter().any(|p| p.style.bold));
        assert!(lines[1].pieces.iter().all(|p| p.style.bold));
    }

    #[test]
    fn indents_list_items_and_hangs_continuations() {
        let block = Block {
            kind: BlockKind::ListItem {
                depth: 0,
                ordinal: Some(1),
            },
            runs: vec![Run {
                text: "first item that wraps".into(),
                style: RunStyle::default(),
            }],
        };
        let lines = layout_full(
            &chapter_of(vec![block]),
            100,
            LayoutOptions { max_width: 20 },
            &[],
        );
        assert_eq!(lines[0].indent, 2);
        assert_eq!(lines[0].pieces[0].text, "1. ");
        assert!(lines[1].indent > lines[0].indent);
    }

    #[test]
    fn never_drops_text() {
        let source = "The quick brown fox jumps over the lazy dog again and again";
        let chapter = chapter_of(vec![paragraph(source)]);
        for width in [20u16, 25, 33, 47, 66] {
            let lines = layout_full(&chapter, 100, LayoutOptions { max_width: width }, &[]);
            let joined: Vec<String> = lines
                .iter()
                .filter(|l| l.kind != LineKind::Blank)
                .map(|l| l.pieces.iter().map(|p| p.text.as_str()).collect())
                .collect();
            assert_eq!(joined.join(" "), source, "width {width}");
        }
    }
}
