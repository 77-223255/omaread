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

    /// True when the cursor may stand here.
    ///
    /// Text lines and a picture's own rows are stops, so scrolling walks
    /// through a picture a row at a time and it gets its chance to appear
    /// whole. The gap between two paragraphs and a rule are nobodies: the
    /// cursor steps over them.
    pub fn is_cursor_stop(&self) -> bool {
        self.text_len() > 0 || matches!(self.kind, LineKind::Image { .. })
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
            if !line.is_cursor_stop() {
                continue;
            }
            if line.block < block || (line.block == block && line.offset <= offset) {
                best = Some(index);
            } else {
                break;
            }
        }
        best.or_else(|| self.lines.iter().position(|l| l.is_cursor_stop()))
    }

    /// The first cursor stop at or after `from`.
    pub fn next_selectable(&self, from: usize) -> Option<usize> {
        self.lines
            .iter()
            .enumerate()
            .skip(from)
            .find(|(_, l)| l.is_cursor_stop())
            .map(|(i, _)| i)
    }

    /// The last cursor stop at or before `from`.
    pub fn previous_selectable(&self, from: usize) -> Option<usize> {
        self.lines
            .iter()
            .enumerate()
            .take(from + 1)
            .rfind(|(_, l)| l.is_cursor_stop())
            .map(|(i, _)| i)
    }
}

/// Lays out a chapter with room reserved for pictures.
///
/// `images` must arrive in ascending block order, as `Pictures::placements`
/// builds them: one forward walk finds every picture's place, where searching
/// from the first placement for each picture cost a chapter of hundreds of
/// them quadratic.
pub fn layout_full(
    chapter: &Chapter,
    available_width: u16,
    images: &[ImagePlacement],
) -> Vec<Line> {
    let width = available_width.max(1);
    let mut lines: Vec<Line> = Vec::new();
    debug_assert!(
        images.windows(2).all(|pair| pair[0].block <= pair[1].block),
        "picture placements must arrive in block order"
    );
    // Where the walk stands: blocks are visited in order, so each picture's
    // placement is either at the cursor or further on, never behind it.
    let mut placed = 0usize;

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
            BlockKind::Image { .. } => {
                // Walk up to this block's own placement, if it has one.
                while placed < images.len() && images[placed].block < index {
                    placed += 1;
                }
                match images.get(placed).filter(|image| image.block == index) {
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
                }
            }
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

/// Wraps one block of text, and stretches the lines of a paragraph as the
/// options ask.
#[allow(clippy::too_many_arguments)]
/// Wraps one block of text.
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
        let end = forbid_breaks(&chars, at, break_at(&chars, at, budget));
        let mut pieces = Vec::new();
        if first
            && let Some(marker) = &marker {
                pieces.push(Piece::decoration(marker.clone()));
            }
        pieces.extend(styles.pieces(&chars, at, end));
        // A break the author marked with a soft hyphen shows the hyphen at the
        // end of the line it left; the mark itself is never drawn.
        if chars.get(end) == Some(&SOFT_HYPHEN) {
            pieces.push(Piece::decoration("-".to_string()));
        }

        out.push(Line {
            block: index,
            offset: at,
            indent: if first { indent } else { indent + marker_width },
            kind,
            pieces,
        });

        at = next_start(&chars, end);
        first = false;
    }
}

/// A soft hyphen: a break the author allows, shown as `-` when it is taken.
const SOFT_HYPHEN: char = '\u{ad}';

/// Finds where to break a line: the last space that fits, a soft hyphen the
/// author set, or a separator inside a long word or address — whichever the
/// text offers closest to the end.
fn break_at(chars: &[char], start: usize, budget: u16) -> usize {
    let budget = budget as usize;
    let mut width = 0usize;
    let mut last_space: Option<usize> = None;
    let mut last_soft: Option<usize> = None;
    let mut last_url: Option<usize> = None;
    // The last point where a Han line could end. A mixed line may break
    // between two such characters anywhere, so this stays a candidate on its
    // own and does not send the break back to a space far behind.
    let mut last_cjk: Option<usize> = None;
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
            // The last opportunity the text offered wins; cutting a word in
            // half is the last resort. A separator inside the word counts
            // only when the word could not fit on a line of its own: moving it
            // whole to the next line reads better than leaving `node.` at the
            // edge and `js` after it.
            let url = last_url.filter(|_| word_overflows(chars, start, last_space, budget));
            return [last_space, last_soft, url, last_cjk]
                .into_iter()
                .flatten()
                .filter(|break_at| *break_at > start)
                .max()
                .unwrap_or_else(|| at.max(start + 1));
        }
        if ch == ' ' {
            last_space = Some(at);
        } else if ch == SOFT_HYPHEN {
            last_soft = Some(at);
        } else if breaks_word(ch) {
            // Break after the separator, the way an address is broken.
            last_url = Some(at + 1);
        }
        width += w;
        at += 1;
        // The boundary that now sits at `at` is a break a Han line may take.
        if at < chars.len()
            && at > start
            && (crate::doc::is_cjk(chars[at - 1]) || crate::doc::is_cjk(chars[at]))
        {
            last_cjk = Some(at);
        }
    }
    chars.len()
}

/// Whether the word the overflow sits in is longer than one whole line.
///
/// A separator inside a word is a break the reader can accept only when the
/// word cannot fit anywhere whole: otherwise the break belongs at the space
/// before it and the word moves to the next line intact.
fn word_overflows(chars: &[char], start: usize, last_space: Option<usize>, budget: usize) -> bool {
    let from = last_space.map_or(start, |space| space + 1);
    let mut width = 0usize;
    for &ch in &chars[from..] {
        if ch == ' ' {
            break;
        }
        width += ch.width().unwrap_or(0);
        if width > budget {
            return true;
        }
    }
    false
}

/// Characters a long word or an address may be broken after when there is no
/// space to break at. Cutting at one of these reads far better than cutting
/// the word itself in half.
fn breaks_word(ch: char) -> bool {
    matches!(
        ch,
        '-' | '/' | '.' | '_' | '?' | '&' | '=' | '+' | '%' | '#' | '~' | ':' | '@'
    )
}

/// Moves a break away from a mark that may not stand at a line's edge.
///
/// Chinese and Japanese typesetting forbids a line to begin with a closing
/// mark — a comma or a bracket whose text is on the line before — or to end
/// with an opening one. The break moves one character earlier in both cases,
/// so the mark travels with its neighbour rather than dangling at the edge.
fn forbid_breaks(chars: &[char], start: usize, end: usize) -> usize {
    let mut end = end;
    loop {
        if end <= start + 1 {
            return end;
        }
        if end < chars.len() && cannot_start_line(chars[end]) {
            end -= 1;
            continue;
        }
        if cannot_end_line(chars[end - 1]) {
            end -= 1;
            continue;
        }
        return end;
    }
}

/// Marks that may not begin a line: they close what came before.
fn cannot_start_line(ch: char) -> bool {
    matches!(
        ch,
        '。' | '，'
            | '、'
            | '．'
            | '；'
            | '：'
            | '！'
            | '？'
            | '）'
            | '〕'
            | '〉'
            | '》'
            | '」'
            | '』'
            | '】'
            | '〗'
            | '〙'
            | '〛'
            | '｝'
            | '”'
            | '’'
            | '…'
            | '—'
            | '～'
            | '·'
    )
}

/// Marks that may not end a line: they open what follows.
fn cannot_end_line(ch: char) -> bool {
    matches!(
        ch,
        '（' | '〔'
            | '〈'
            | '《'
            | '「'
            | '『'
            | '【'
            | '〖'
            | '〘'
            | '〚'
            | '｛'
            | '“'
            | '‘'
    )
}

/// Where the next line starts: past the spaces a break dropped and past the
/// soft hyphen it broke at.
fn next_start(chars: &[char], mut at: usize) -> usize {
    while at < chars.len() && (chars[at] == ' ' || chars[at] == SOFT_HYPHEN) {
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
    fn the_cursor_stops_on_a_pictures_rows() {
        // The cursor steps through a picture's own lines, not over them: the
        // view follows the cursor, so a picture the cursor jumps across in one
        // step is a picture the reader never gets to see whole.
        let chapter = chapter_of(vec![paragraph("before"), picture(), paragraph("after")]);
        let lines = layout_full(
            &chapter,
            40,
            &[ImagePlacement {
                block: 1,
                rows: 3,
                cols: 6,
            }],
        );
        let index = Index::new(&lines);
        let start = index.line_of(0, 0).expect("the text line");

        // Down through the three rows, then on to the text after them.
        let mut at = start;
        for row in 0..3u16 {
            at = index.next_selectable(at + 1).expect("a row of the picture");
            assert!(
                matches!(lines[at].kind, LineKind::Image { row: r, .. } if r == row),
                "expected the picture's row {row}"
            );
        }
        let after = index.next_selectable(at + 1).expect("the text after");
        assert!(!matches!(lines[after].kind, LineKind::Image { .. }));

        // And back up the same way.
        let back = index.previous_selectable(after - 1).expect("the last row");
        assert!(matches!(lines[back].kind, LineKind::Image { row: 2, .. }));
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
    fn every_picture_finds_its_own_placement() {
        // Placements are walked to rather than searched for from the first
        // one each time — the walk replaces an O(images) find per picture —
        // so the expectation is written out line by line by hand: a cursor
        // that stepped over the wrong placement, or an unplaced picture that
        // took the next one's rows, would show against it.
        let chapter = chapter_of(vec![
            paragraph("before"),
            picture(),
            paragraph("mid"),
            picture(),
            picture(),
            paragraph("after"),
            picture(),
        ]);
        let images = vec![
            ImagePlacement {
                block: 1,
                rows: 2,
                cols: 4,
            },
            ImagePlacement {
                block: 3,
                rows: 1,
                cols: 4,
            },
            ImagePlacement {
                block: 4,
                rows: 3,
                cols: 6,
            },
        ];
        let lines = layout_full(&chapter, 10, &images);

        // Every line as (block, what it is, how far in it starts).
        let shape: Vec<(usize, String, u16)> = lines
            .iter()
            .map(|line| {
                let what = match line.kind {
                    LineKind::Blank => "blank".to_string(),
                    LineKind::Body => "body".to_string(),
                    LineKind::Image { row, rows } => format!("picture {row}/{rows}"),
                    other => format!("{other:?}"),
                };
                (line.block, what, line.indent)
            })
            .collect();
        let row = |block: usize, what: &str, indent: u16| (block, what.to_string(), indent);
        assert_eq!(
            shape,
            vec![
                row(0, "body", 0),
                row(1, "blank", 0),
                row(1, "picture 0/2", 3),
                row(1, "picture 1/2", 3),
                row(2, "blank", 0),
                row(2, "body", 0),
                row(3, "blank", 0),
                row(3, "picture 0/1", 3),
                row(4, "blank", 0),
                row(4, "picture 0/3", 2),
                row(4, "picture 1/3", 2),
                row(4, "picture 2/3", 2),
                row(5, "blank", 0),
                row(5, "body", 0),
                // Block 6 has no placement: its alt text is the one row, at
                // the block's own indent, and it took nobody else's rows.
                row(6, "blank", 0),
                row(6, "picture 0/1", 0),
            ],
            "the layout no longer places each picture where it belongs"
        );
    }

    #[test]
    fn a_word_that_fits_on_a_line_of_its_own_is_not_cut_at_its_own_dots() {
        // `node.js@latest` fits in a line, so the break belongs at the space
        // before it and the word moves whole. Taking the last separator
        // inside it instead would leave `node.` at the end of one line and
        // `js@latest` at the start of the next, which reads as two words.
        let chapter = chapter_of(vec![paragraph("Install node.js@latest now")]);
        let lines = layout_full(&chapter, 16, &[]);
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.pieces.iter().map(|p| p.text.as_str()).collect())
            .collect();
        assert_eq!(texts, vec!["Install", "node.js@latest", "now"]);
    }

    #[test]
    fn a_han_line_ends_where_the_budget_does_not_at_the_last_space() {
        // A mixed line may break between two Han characters wherever it runs
        // out of room. Falling back to the last space — here the one around
        // `·`, far behind — leaves the line half empty and pushes a whole
        // clause to the next one.
        let chapter = chapter_of(vec![paragraph(
            "大学最后一年，肯选了埃尔温 · 伯利坎普（Elwyn Berlekamp）的课。",
        )]);
        let lines = layout_full(&chapter, 36, &[]);
        let first: String = lines[0].pieces.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(first, "大学最后一年，肯选了埃尔温 · 伯利坎");
    }

    #[test]
    fn a_tilde_between_han_characters_does_not_end_a_line() {
        // `6~8` is one range: the tilde is only a break for a long word or an
        // address, not for the middle of a Chinese sentence.
        let chapter = chapter_of(vec![paragraph(
            "招聘官一试再试。如肯所言：“贝尔实验室问了6~8次，我都拒绝了。”",
        )]);
        let lines = layout_full(&chapter, 36, &[]);
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.pieces.iter().map(|p| p.text.as_str()).collect())
            .collect();
        assert!(
            !texts.iter().any(|text| text.ends_with('~')),
            "the tilde was left at a line end: {texts:?}"
        );
    }

    #[test]
    fn wraps_at_word_boundaries_and_records_where_each_line_starts() {
        let chapter = chapter_of(vec![paragraph("aaa bbb ccc ddd")]);
        let lines = layout_full(&chapter, 11, &[]);
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
        let lines = layout_full(&chapter, 24, &[]);
        // Minimum usable width is 20, so this fits on one line.
        assert_eq!(lines.len(), 1);

        let chapter = chapter_of(vec![paragraph(&"x".repeat(50))]);
        let lines = layout_full(&chapter, 20, &[]);
        assert!(lines.len() > 1);
        assert!(lines.iter().all(|l| !l.pieces.is_empty()));
    }

    #[test]
    fn separates_paragraphs_with_a_blank_line() {
        let chapter = chapter_of(vec![paragraph("one"), paragraph("two")]);
        let lines = layout_full(&chapter, 100, &[]);
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
        let lines = layout_full(&chapter_of(vec![block]), 12, &[]);
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
        let lines = layout_full(&chapter_of(vec![block]), 20, &[]);
        assert_eq!(lines[0].indent, 2);
        assert_eq!(lines[0].pieces[0].text, "1. ");
        assert!(lines[1].indent > lines[0].indent);
    }

    #[test]
    fn never_drops_text() {
        let source = "The quick brown fox jumps over the lazy dog again and again";
        let chapter = chapter_of(vec![paragraph(source)]);
        for width in [20u16, 25, 33, 47, 66] {
            let lines = layout_full(&chapter, width, &[]);
            // Every character the book wrote is still there, in order.
            let joined: Vec<String> = lines
                .iter()
                .filter(|l| l.kind != LineKind::Blank)
                .map(|l| {
                    l.pieces
                        .iter()
                        .filter(|p| !p.decoration)
                        .map(|p| p.text.as_str())
                        .collect()
                })
                .collect();
            assert_eq!(joined.join(" "), source, "width {width}");
        }
    }

    #[test]
    fn chinese_punctuation_does_not_dangle_at_a_line_edge() {
        // A closing mark travels with the character it closes: the comma never
        // begins a line, even though it would fit at the end of this one.
        let chars: Vec<char> = "一二三，四".chars().collect();
        assert_eq!(forbid_breaks(&chars, 0, 3), 2);
        // And an opener never ends one: it travels on with what it opens.
        let chars: Vec<char> = "一二三（四".chars().collect();
        assert_eq!(forbid_breaks(&chars, 0, 4), 3);

        // Laid out, the comma stays with the character before it.
        let chapter = chapter_of(vec![paragraph("一二三四五六七八九，十")]);
        let lines = layout_full(&chapter, 18, &[]);
        let text = |line: &Line| -> String {
            line.pieces
                .iter()
                .filter(|piece| !piece.decoration)
                .map(|piece| piece.text.as_str())
                .collect()
        };
        assert_eq!(text(&lines[0]), "一二三四五六七八");
        assert_eq!(text(&lines[1]), "九，十");
    }

    #[test]
    fn a_soft_hyphen_offers_a_break_and_shows_the_hyphen() {
        let chapter = chapter_of(vec![paragraph("hyphen\u{ad}ation here")]);
        let lines = layout_full(&chapter, 7, &[]);
        let text = |line: &Line| -> String {
            line.pieces
                .iter()
                .filter(|piece| !piece.decoration)
                .map(|piece| piece.text.as_str())
                .collect()
        };
        assert_eq!(text(&lines[0]), "hyphen");
        assert!(
            lines[0]
                .pieces
                .iter()
                .any(|piece| piece.decoration && piece.text == "-"),
            "the hyphen is shown where the break was taken: {:?}",
            lines[0].pieces
        );
        assert_eq!(text(&lines[1]), "ation");
    }

    #[test]
    fn a_long_word_breaks_at_its_separators_before_it_breaks_in_half() {
        let chars: Vec<char> = "https://example.com/path".chars().collect();
        let end = break_at(&chars, 0, 12);
        assert_eq!(&chars[..end].iter().collect::<String>(), "https://");

        // A word with nothing to break at is still cut, as a last resort.
        let chars: Vec<char> = "abcdefghijklmnop".chars().collect();
        assert_eq!(break_at(&chars, 0, 5), 5);
    }
}



