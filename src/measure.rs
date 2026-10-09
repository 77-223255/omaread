//! Measuring text in terminal cells.
//!
//! The reader counts cells, never characters: a row cut by character count
//! drifts a cell to the right for every wide glyph in it. These are the plain
//! measurements every view shares — the shelf's columns, the status row, the
//! command line's own output — so they live apart from any one view that
//! draws them.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// The text with its runs of whitespace collapsed to one space, then cut to
/// the width. A line that already fits and has no run to collapse is handed
/// back as it is: the search's snippets are usually one tidy line of prose,
/// and splitting and rejoining them would only build the same string again.
pub(crate) fn shorten(text: &str, width: usize) -> String {
    if cells(text) <= width && !text.chars().any(char::is_whitespace) {
        return text.to_string();
    }
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
    if width == 0 {
        return String::new();
    }
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

/// The text cut to fit a number of cells, with a mark where it was cut. Text
/// that already fits is handed back untouched.
pub(crate) fn fit(text: &str, width: usize) -> String {
    if cells(text) <= width {
        text.to_string()
    } else {
        cut(text, width)
    }
}

/// Cuts or pads a value to a fixed width, so the columns line up.
pub(crate) fn pad(text: &str, width: usize) -> String {
    let used = cells(text);
    if used > width {
        return cut(text, width);
    }
    format!("{text}{}", " ".repeat(width - used))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn text_is_cut_only_when_it_does_not_fit() {
        assert_eq!(fit("short", 10), "short");
        assert_eq!(
            fit("一九七三年的弹子球", 12),
            cut("一九七三年的弹子球", 12),
            "a text that overflows is marked where it was cut"
        );
        assert_eq!(fit("anything", 0), "", "no room says nothing at all");
    }
}
