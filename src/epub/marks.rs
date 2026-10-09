//! Reading a chapter: parsing it, and closing the text over its marks.
//!
//! A chapter is parsed once, when it is opened, and the marks are hidden
//! before anything else looks at it — the view, the search and the layout all
//! read the text as the reader does, with the marks already folded away.

use super::Book;
use crate::doc::Chapter;

/// Loads a chapter, turning a parse failure into a readable placeholder rather
/// than ending the session. The marks are hidden once the chapter is there, so
/// the text the reader gets has already closed over them.
pub fn load_chapter(book: &mut Book, index: usize, cell: crate::image::CellSize) -> Chapter {
    let mut chapter = match book.chapter(index) {
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
    };
    hide_marks(book, &mut chapter, cell);
    chapter
}

/// Hides the marks in a chapter and closes the text over them.
///
/// A mark — a glyph-sized picture the book uses where a character would be,
/// such as a footnote's marker — is not drawn. Left where it is, its block
/// would still break the sentence in two, so the block goes and the paragraph
/// on its far side is folded back into the one before it: the sentence reads
/// on, wrapped by the layout like any other. A mark that is not bracketed by
/// two paragraphs is simply dropped.
///
/// Taking a block out shifts every later block, and folding one moves every
/// character in it, so the links and the anchors are moved with the text.
fn hide_marks(book: &mut Book, chapter: &mut Chapter, cell: crate::image::CellSize) {
    let hidden: Vec<bool> = chapter
        .blocks
        .iter()
        .map(|block| match &block.kind {
            crate::doc::BlockKind::Image { src: Some(src) } => {
                book.picture_reason(src).role() == crate::image::Role::Keep
                    // Through the book's own cache: the layout measures the
                    // same pictures again the moment this chapter is laid
                    // out, and a header read twice per picture per chapter
                    // load is the same work twice for the same answer.
                    && book
                        .picture_size(src)
                        .is_some_and(|size| crate::image::is_mark(size, cell))
            }
            _ => false,
        })
        .collect();
    if !hidden.iter().any(|mark| *mark) {
        return;
    }

    let old = std::mem::take(&mut chapter.blocks);
    let mut blocks: Vec<crate::doc::Block> = Vec::with_capacity(old.len());
    // Where every original block went: the block it now lives in, and how
    // many characters of that block stand before its own start.
    let mut moved: Vec<(usize, usize)> = Vec::with_capacity(old.len());
    let mut index = 0;
    while index < old.len() {
        if !hidden[index] {
            moved.push((blocks.len(), 0));
            blocks.push(old[index].clone());
            index += 1;
            continue;
        }
        // The mark itself is gone. Between two paragraphs, the second folds
        // into the first, so the sentence it interrupted reads on.
        let folds = matches!(
            (blocks.last(), old.get(index + 1)),
            (Some(before), Some(after))
                if before.kind == crate::doc::BlockKind::Paragraph
                    && after.kind == crate::doc::BlockKind::Paragraph
        );
        moved.push((usize::MAX, 0));
        if folds {
            let before = blocks.last_mut().expect("just matched a block");
            let delta = fold_paragraph(before, &old[index + 1]);
            moved.push((blocks.len() - 1, delta));
            index += 2;
        } else {
            index += 1;
        }
    }

    for link in &mut chapter.links {
        if let Some((block, add)) = moved.get(link.block).copied()
            && block != usize::MAX
        {
            link.block = block;
            link.start += add;
            link.end += add;
        }
    }
    for (block, offset) in chapter.anchors.values_mut() {
        if let Some((new_block, add)) = moved.get(*block).copied()
            && new_block != usize::MAX
        {
            *block = new_block;
            *offset += add;
        }
    }
    chapter.blocks = blocks;
}

/// Folds the second paragraph into the first, dropping the air between them
/// when both sides bring some and putting a space back between two words the
/// parser trimmed it from. Answers how many characters of the merged text
/// stand before the second paragraph's own start, which is where its links
/// and anchors move to.
fn fold_paragraph(into: &mut crate::doc::Block, from: &crate::doc::Block) -> usize {
    let into_len = into.plain_text().chars().count();
    let mut runs = from.runs.clone();
    let mut dropped = 0;
    let mut inserted = 0;
    let before = into.plain_text();
    let after = from.plain_text();
    if before.ends_with(char::is_whitespace) {
        // Both sides bring air; one space is enough.
        if let Some(first) = runs.iter_mut().find(|run| !run.text.is_empty()) {
            let trimmed = first.text.trim_start().to_string();
            dropped = first.text.chars().count() - trimmed.chars().count();
            first.text = trimmed;
        }
    } else if !after.starts_with(char::is_whitespace)
        && let (Some(last), Some(first)) = (before.chars().next_back(), after.chars().next())
        && !crate::doc::is_cjk(last)
        && !crate::doc::is_cjk(first)
    {
        // A space the parser trimmed from both sides of the mark: two Latin
        // words still need one between them, where two Han characters do not.
        into.runs.push(crate::doc::Run {
            text: " ".to_string(),
            style: crate::doc::RunStyle::default(),
        });
        inserted = 1;
    }
    into.runs
        .extend(runs.into_iter().filter(|run| !run.text.is_empty()));
    into_len + inserted - dropped
}

#[cfg(test)]
mod tests {
    use crate::app::App;
    use crate::testkit::{book_with, opf, png};

    /// A book whose one chapter has a picture of the given size in the middle
    /// of a sentence: small enough to be a footnote's glyph, which the reader
    /// hides, or large enough to be a real figure, which it keeps.
    fn img_book(
        name: &str,
        width: u32,
        height: u32,
        what: &str,
    ) -> std::path::PathBuf {
        book_with(
            name,
            "Mark",
            &[&format!(
                r#"<p>before the {what} <img src="img.png" alt="{what}"/> and after it</p>"#
            )],
            &[
                (
                    "OEBPS/content.opf",
                    &opf(
                        "Mark",
                        r#"<item id="img" href="img.png" media-type="image/png"/>"#,
                    ),
                ),
                ("OEBPS/img.png", &png(width, height, [200, 100, 50, 255])),
            ],
        )
    }

    /// The text of what is on screen, without the decoration the layout adds.
    fn visible_text(app: &App) -> String {
        app.visible_lines()
            .iter()
            .flat_map(|line| {
                line.pieces
                    .iter()
                    .filter(|piece| !piece.decoration)
                    .map(|piece| piece.text.as_str())
            })
            .collect()
    }

    #[test]
    fn a_phones_larger_cells_do_not_hide_a_figure() {
        // Judged in cells alone a 200x120 picture is 25x8 on a desktop but
        // 5x3 at the 40-pixel cells a phone reports: three rows tall, which
        // the mark rule used to read as a footnote glyph and hide. Pixels
        // have the last word, so the figure keeps its line.
        let path = img_book("app-hide-figure", 200, 120, "figure");
        let phone = crate::image::CellSize {
            width: 40,
            height: 40,
        };
        let (_dir, mut app) = crate::testapp::opened("app-hide-figure-journal", &path, phone);
        app.prepare(80, 10);

        assert!(
            app.visible_lines()
                .iter()
                .any(|line| matches!(line.kind, crate::layout::LineKind::Image { .. })),
            "the figure was hidden as a mark on a phone-sized cell"
        );
        assert!(visible_text(&app).contains("before the figure"));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn a_hidden_mark_joins_the_sentence_it_interrupts() {
        // The mark is not drawn, and the paragraph after it is folded into
        // the one before, so the sentence reads on in one flow rather than
        // breaking at a picture that is not there.
        let path = img_book("app-hide-mark", 48, 48, "mark");
        let (_dir, mut app) = crate::testapp::opened(
            "app-hide-mark-journal",
            &path,
            crate::image::CellSize::default(),
        );
        app.prepare(80, 10);

        assert_eq!(visible_text(&app), "before the mark and after it");
        assert!(
            app.visible_lines()
                .iter()
                .all(|line| !matches!(line.kind, crate::layout::LineKind::Image { .. })),
            "the hidden mark left a line behind"
        );
        std::fs::remove_file(path).ok();
    }
}
