//! Encoding pictures for the Kitty graphics protocol.
//!
//! `a=T` transmits the payload and displays it in one escape, which is the
//! form every terminal that speaks the protocol understands. The payload has
//! to be split into chunks of at most 4096 base64 characters, each carrying
//! `m=1` while more follows and `m=0` on the last one.
//!
//! A picture the view has scrolled into is placed through its visible rows
//! only: the escape names the rectangle of the source those rows cover, so a
//! half-seen picture shows its own top (or bottom) rather than being squeezed
//! into the room that is left, or not drawn at all. The whole picture, which
//! is the common case, pays nothing for that: no source rectangle, and the
//! payload is the one cached when it was rendered.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

const CHUNK: usize = 4096;

/// Builds the escape that draws `png` at the cursor, in `cols` by `visible`
/// cells, showing the rows `from..from + visible` of a picture `rows` tall.
///
/// `source` is the payload's own pixel size, which the crop is measured in.
pub fn place(
    png: &[u8],
    cols: u16,
    rows: u16,
    id: u32,
    from: u16,
    visible: u16,
    source: (u32, u32),
) -> String {
    let payload = STANDARD.encode(png);
    let mut out = String::with_capacity(payload.len() + 256);
    let mut chunks = payload.as_bytes().chunks(CHUNK).peekable();
    let mut first = true;

    while let Some(chunk) = chunks.next() {
        let more = u8::from(chunks.peek().is_some());
        out.push_str("\x1b_G");
        if first {
            // f=100 announces PNG; a=T sends it and draws it in one go; C=1
            // keeps the cursor where it is, so the text layout is untouched,
            // and q=2 keeps the terminal quiet on a channel nobody reads.
            let crop = if from == 0 && visible >= rows {
                String::new()
            } else {
                let (source_w, source_h) = source;
                let rows = rows.max(1) as u32;
                let y = from as u32 * source_h / rows;
                let h = ((from as u32 + visible as u32) * source_h / rows)
                    .saturating_sub(y)
                    .max(1);
                // The source rectangle is in the payload's own pixels: the
                // rows the view still shows, of the picture it holds whole.
                format!(",x=0,y={y},w={source_w},h={h}")
            };
            out.push_str(&format!(
                "i={id},f=100,a=T,c={cols},r={visible}{crop},C=1,q=2,m={more}"
            ));
            first = false;
        } else {
            out.push_str(&format!("m={more}"));
        }
        out.push(';');
        out.push_str(&String::from_utf8_lossy(chunk));
        out.push_str("\x1b\\");
    }
    out
}

/// Removes every picture this reader placed, before a frame paints the ones
/// it wants.
///
/// `d=A` takes all of them at once: the pictures are placed in reading order
/// every frame that moved one, so there is nothing to gain from naming the
/// ids one by one — and a stale picture under an id this reader no longer
/// remembers would otherwise stay on screen forever.
pub fn delete_all() -> &'static str {
    "\x1b_Ga=d,d=A,q=2\x1b\\"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_payload_travels_in_chunks_and_comes_back() {
        // A small image is one chunk carrying every parameter.
        let escape = place(b"tiny", 10, 5, 7, 0, 5, (200, 100));
        assert_eq!(escape.matches("\x1b_G").count(), 1);
        assert!(escape.contains("f=100,a=T,c=10,r=5"), "{escape}");
        assert!(escape.contains("m=0"));
        assert!(escape.ends_with("\x1b\\"));
        // The whole picture names no source rectangle.
        assert!(!escape.contains("x=0"), "{escape}");

        // A payload longer than one chunk carries `m=1` until the last one.
        let big = vec![7u8; CHUNK * 3 / 4 * 2];
        let escape = place(&big, 4, 2, 7, 0, 2, (8, 4));
        assert!(escape.contains("m=1;"));
        assert!(escape.trim_end().ends_with("\x1b\\"));
        assert!(!escape.ends_with("m=0;"), "the last chunk carries data");
        assert!(escape.matches("\x1b_G").count() > 1);
    }

    #[test]
    fn a_cropped_placement_names_the_rows_left() {
        // 8 rows of a 16-row picture, starting at row 4: a quarter down the
        // payload, half its height.
        let escape = place(b"tiny", 10, 16, 7, 4, 8, (100, 200));
        assert!(escape.contains("c=10,r=8"), "{escape}");
        assert!(escape.contains("x=0,y=50,w=100,h=100"), "{escape}");
    }

    #[test]
    fn the_clear_takes_every_picture_at_once() {
        assert_eq!(delete_all(), "\x1b_Ga=d,d=A,q=2\x1b\\");
    }
}
