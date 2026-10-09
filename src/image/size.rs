//! Sizing a picture for the room it is drawn in.
//!
//! Everything here answers from a header and a cell size — no decoding, no
//! pixels — because the layout asks for every picture of a chapter before a
//! single one is drawn.

/// How big a picture is drawn: whether it fills the room or answers to what
/// it is itself. Two answers, because there are two rules and no third.
///
/// What the picture *is* in the book — cover, page, flow — is the reason for
/// the answer, carried separately in `Reason` so a report can name it without
/// the two rules growing a third name each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Fills the room, aspect kept, magnified within `MAGNIFY` to do it.
    Fill,
    /// A picture in the flow of text: a mark keeps its own size in cells —
    /// pixels divided by the terminal's cell — and anything larger is
    /// magnified within `MAX_UPSCALE` to fill the room. Whichever it is, the
    /// room caps it and it is never drawn past the room.
    Keep,
}

/// What a picture is in its book: the reason behind its `Role`.
///
/// Real readers classify pictures structurally, not by how they look: the
/// cover, the page of a pre-paginated book, and everything else. The first
/// two fill the room because a screen is as big as the screen; the third
/// sits in a flow of text, where a mark stays the glyph it is and an
/// illustration in the body of the book grows into the room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The cover, named by the package document.
    Cover,
    /// One whole page of a pre-paginated book (manga, picture books): one
    /// spine item is exactly one page.
    Page,
    /// A picture in the flow of text — a diagram, a plate, a footnote's mark.
    Flow,
}

impl Reason {
    /// The sizing rule this reason asks for.
    pub fn role(&self) -> Role {
        match self {
            Reason::Cover | Reason::Page => Role::Fill,
            Reason::Flow => Role::Keep,
        }
    }

}

/// The most a filling picture may be magnified, in multiples of its own size
/// in cells.
///
/// Without this the book decides the whole screen: `properties="cover-image"`
/// on a 1×1 pixel, or a single `rendition:layout` line on an ordinary book,
/// made every picture fill the room. Four is a small enough factor that a
/// tiny mark stays a mark and a real cover still fills a screen, and it is
/// the same cap for both filling rules — cover and page ask for no more. A
/// picture in the body of a book is magnified by `MAX_UPSCALE` instead: two
/// constants for two rules, so one may move without moving the other.
const MAGNIFY: u16 = 4;

/// The most an illustration in the body of a book is magnified, in multiples
/// of its own size in cells. Four times is what a mid-sized picture needs to
/// reach the width of a page; past that, a page drawn by hand is a page
/// nobody drew.
const MAX_UPSCALE: u16 = 4;

/// A picture no taller than this many rows of text, and no wider than
/// `MARK_COLS` cells, is a mark: a glyph-sized image the book uses where a
/// character would be, such as a footnote's marker. A mark is hidden rather
/// than drawn — a line of icon on its own says less than the sentence around
/// it reads closed over it — and `is_mark` is the one place that decides what
/// one is.
const MARK_ROWS: u16 = 3;
const MARK_COLS: u16 = 20;

/// The largest a mark may be, in pixels on either side. A footnote glyph is a
/// glyph whatever the terminal's cells are, so this bound is what keeps a
/// real figure from being taken for one on a screen with large cells.
const MARK_PIXELS: u32 = 64;

/// Size of one terminal cell in pixels. Needed to scale a picture to a whole
/// number of cells, so the reserved lines match what the terminal paints.
#[derive(Debug, Clone, Copy)]
pub struct CellSize {
    pub width: u16,
    pub height: u16,
}

impl Default for CellSize {
    fn default() -> Self {
        // A common default; the real size is read from the terminal when it
        // reports one.
        Self {
            width: 8,
            height: 16,
        }
    }
}

/// The picture's own size in cells: its pixels divided by one cell, each side
/// rounded up, because a cell is the smallest thing a screen can hold.
fn natural_size((width, height): (u32, u32), cell: CellSize) -> (u16, u16) {
    (
        (width as f32 / cell.width.max(1) as f32).ceil().max(1.0) as u16,
        (height as f32 / cell.height.max(1) as f32).ceil().max(1.0) as u16,
    )
}

/// Whether a picture is a mark: a glyph-sized image in the flow of text, which
/// the reader hides rather than draws.
///
/// The test is here, beside the size that defines it, so the reader and the
/// layout cannot disagree about what one is. It is only meaningful for
/// `Role::Keep`: a cover or a page is a screen whatever its pixels.
///
/// Pixels decide first, and cells only confirm. A footnote glyph is drawn at
/// glyph size in pixels whatever the terminal is; a phone's cells are several
/// times a desktop's, so judged in cells alone a 200x120 diagram could come
/// out three rows tall and be hidden.
pub fn is_mark(size: (u32, u32), cell: CellSize) -> bool {
    let (width, height) = size;
    if width == 0 || height == 0 || width > MARK_PIXELS || height > MARK_PIXELS {
        return false;
    }
    let (cols, rows) = natural_size(size, cell);
    marks(cols, rows)
}

/// Whether a picture of this size in cells is a mark.
fn marks(cols: u16, rows: u16) -> bool {
    rows <= MARK_ROWS && cols <= MARK_COLS
}

/// The cell box a picture will occupy, from its pixel size alone.
///
/// How a picture is sized follows from what it is in the book, and there are
/// two answers:
///
/// - **Fill** — the cover, and a picture that is a whole page of a
///   pre-paginated book — takes the room, aspect preserved, magnified if
///   filling is what it takes, but never past `MAGNIFY` times its own size
///   in cells: a book cannot claim the screen with a pixel. Neither is
///   tested for being a mark — a cover or a page is a screen whatever its
///   pixels.
/// - **Keep** — every picture in the flow of text — answers to what it is
///   itself. A mark, no taller than `MARK_ROWS` and no wider than
///   `MARK_COLS`, keeps **its own size**: its pixels divided by the
///   terminal's cell, only ever capped by the room, never magnified. A cell
///   is about ten pixels wide, so a picture's own size in cells is honest:
///   a 48x48 footnote mark stays the mark its publisher drew. Anything
///   larger is magnified within `MAX_UPSCALE` to fill the room. The reader
///   hides a mark before it ever draws it; the size is answered all the same,
///   so the one rule that decides what a mark is stays in one place.
///
/// The room is the cap on every rule here, so nothing ever overflows the view.
///
/// Shared with `fit` and `fit_pixels` below, so a measured height and a
/// rendered one cannot drift apart.
pub fn measure(
    (width, height): (u32, u32),
    max_cols: u16,
    max_rows: u16,
    role: Role,
    cell: CellSize,
) -> (u16, u16) {
    if width == 0 || height == 0 {
        return (0, 0);
    }
    let cell_w = cell.width.max(1) as f32;
    let cell_h = cell.height.max(1) as f32;
    let (natural_cols, natural_rows) = natural_size((width, height), cell);

    // A mark: a glyph-sized picture where a character would be, which the
    // reader hides rather than draws. Only a picture in the flow of text is
    // tested for it: a cover or a page is a screen whatever its pixels, so it
    // fills the room without being asked whether it happens to be small.
    // Judged by the same rule the reader hides by, pixels first, so a phone's
    // larger cells cannot turn a figure into a mark.
    let mark = is_mark((width, height), cell);

    // A mark keeps its own size, whatever room there is: the reader hides it
    // before drawing, and a test of the sizing rule still gets the size it
    // would have had. Everything else is drawn bigger than it is: the cover,
    // the page, and an illustration in the body of the book, which fills the
    // room. Never more than this rule's own magnification times the picture's
    // own size in cells — the room may cap a picture further, and may ask for
    // less than the cap when the picture is already big.
    let (box_cols, box_rows) = if role == Role::Keep && mark {
        (
            natural_cols.min(max_cols.max(1)).max(1),
            natural_rows.min(max_rows.max(1)).max(1),
        )
    } else {
        let upscale = match role {
            Role::Fill => MAGNIFY,
            Role::Keep => MAX_UPSCALE,
        };
        (
            natural_cols
                .saturating_mul(upscale)
                .min(max_cols.max(1))
                .max(1),
            natural_rows
                .saturating_mul(upscale)
                .min(max_rows.max(1))
                .max(1),
        )
    };

    // Fit the box to the room, the shape kept: by the width when the height
    // that needs stays inside the box, by the height otherwise. A cell is not
    // square, so the two sides are compared in pixels, not in cells.
    let rows_needed = (height as f32 * (box_cols as f32 * cell_w) / width as f32) / cell_h;
    if rows_needed <= box_rows as f32 {
        (box_cols, rows_needed.round().max(1.0) as u16)
    } else {
        let cols = (width as f32 * (box_rows as f32 * cell_h) / height as f32) / cell_w;
        ((cols.round().max(1.0) as u16).min(box_cols), box_rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture's own size in cells: its pixels divided by one cell, each
    /// side rounded up. This is what `measure` starts from, so a test that
    /// compares a drawn size with the size it grew from asks for the number
    /// directly instead of borrowing `Role::Keep`, which magnifies a body
    /// picture like anything else.
    fn own_size((width, height): (u32, u32), cell: CellSize) -> (u16, u16) {
        (
            (width as f32 / cell.width.max(1) as f32).ceil().max(1.0) as u16,
            (height as f32 / cell.height.max(1) as f32).ceil().max(1.0) as u16,
        )
    }

    /// What every sized picture must answer to, whichever rule chose its
    /// size: never past the room it is drawn in, and never past four times
    /// its own size in cells — the tighter of the two leashes is the one
    /// that holds.
    fn assert_capped(measured: (u16, u16), own: (u16, u16), room: (u16, u16)) {
        let (cols, rows) = measured;
        assert!(cols <= room.0 && rows <= room.1, "the picture drew {cols}x{rows} in a {room:?} room");
        assert!(cols <= own.0 * 4 && rows <= own.1 * 4, "the picture passed four times its own size {own:?}: {cols}x{rows}");
    }

    #[test]
    fn a_phones_larger_cells_do_not_turn_a_figure_into_a_mark() {
        // The same 200x120 diagram is 25x8 cells on a desktop but 5x3 at the
        // 40-pixel cells a phone reports, and three rows is what the cell
        // count alone calls a mark. Pixels have the last word: a mark is a
        // glyph whatever the screen, so the figure stays a figure.
        let phone = CellSize {
            width: 40,
            height: 40,
        };
        let desktop = CellSize {
            width: 8,
            height: 16,
        };
        assert!(!is_mark((200, 120), phone));
        assert!(!is_mark((200, 120), desktop));
        assert!(is_mark((48, 48), phone), "a glyph is still a glyph");
        assert!(is_mark((48, 48), desktop));
    }

    #[test]
    fn a_picture_always_holds_at_least_one_cell() {
        // The floors: a caller that asks for no room at all, or a picture a
        // single pixel across, still has to reserve and draw something, or the
        // layout and the view disagree about whether it is there.
        let cell = CellSize {
            width: 10,
            height: 31,
        };
        for role in [Role::Fill, Role::Keep] {
            assert_eq!(measure((100, 100), 0, 0, role, cell), (1, 1), "{role:?}");
            let (cols, rows) = measure((1, 1), 120, 40, role, cell);
            assert!(cols >= 1 && rows >= 1, "{role:?} made {cols}x{rows}");
        }
    }

    #[test]
    fn a_footnote_mark_keeps_its_own_size() {
        // 48x48 pixels is a `注` beside a footnote — the size of a footnote's
        // glyph — and its own size is what it keeps, whatever room there is:
        // five cells across by two down at a 10x31 cell, six by three at an
        // 8x16 one. A mark is never magnified, and this is what that means for
        // the smallest thing a book draws — at the 8x16 cell it comes to three
        // rows, the tallest a mark may be and still be a mark.
        assert_eq!(
            measure(
                (48, 48),
                120,
                40,
                Role::Keep,
                CellSize {
                    width: 10,
                    height: 31,
                }
            ),
            (5, 2),
            "at a 10x31 cell"
        );
        assert_eq!(
            measure(
                (48, 48),
                120,
                40,
                Role::Keep,
                CellSize {
                    width: 8,
                    height: 16,
                }
            ),
            (6, 3),
            "at an 8x16 cell"
        );
    }

    #[test]
    fn a_cover_fills_the_room_and_is_capped_at_four_times_its_own_size() {
        // A cover is a screen of its own: aspect kept, scaled up to fill the
        // room — the one case where magnifying is the whole point — but never
        // past four times its own size, or a book that calls a pixel the cover
        // would take the whole screen. The cap does not stop a real cover from
        // filling what four times its own size still covers. The three covers
        // are the three ways a book asks for one: a real picture, a 1x1 pixel
        // named by `properties="cover-image"`, and a mark-sized picture named
        // as a cover — each magnified like one, up to the same four times,
        // wherever the room happens to end.
        let covers = [
            ("a cover", (150, 200), (120, 40), CellSize { width: 10, height: 20 }),
            ("a 1x1 pixel", (1, 1), (80, 20), CellSize::default()),
            ("a mark-sized cover", (48, 48), (120, 40), CellSize { width: 10, height: 31 }),
        ];
        for (note, pixels, room, cell) in covers {
            let own = own_size(pixels, cell);
            let (cols, rows) = measure(pixels, room.0, room.1, Role::Fill, cell);
            assert!(cols >= 1 && rows >= 1, "{note} drew {cols}x{rows}");
            assert!(
                cols > own.0 && rows > own.1,
                "{note} was drawn at its own size {own:?} instead of filling: {cols}x{rows}"
            );
            assert_capped((cols, rows), own, room);
            // What only the real cover proves: it fills the room it was given.
            if pixels == (150, 200) {
                assert_eq!(rows, 40, "the cover does not use the whole height");
            }
        }
    }

    #[test]
    fn a_body_picture_is_magnified_into_the_room_and_stops_at_four_times() {
        // A diagram in the flow of text has one rule: grow into the room —
        // magnified up to four times its own size, capped by the room, and
        // stopped by the four rather than by the room when the room would
        // take a dozen times the picture. Two rooms for one picture: one it
        // nearly fills, one a dozen times its size where the four is the only
        // leash — at which cell its own size comes to is stated exactly.
        let wide = CellSize { width: 10, height: 31 };
        let tall = CellSize { width: 8, height: 16 };
        assert_eq!(
            own_size((300, 400), tall),
            (38, 25),
            "the picture's own size in cells"
        );
        for (room, cell) in [((120, 40), wide), ((400, 200), tall)] {
            let own = own_size((300, 400), cell);
            let (cols, rows) = measure((300, 400), room.0, room.1, Role::Keep, cell);
            assert!(
                cols > own.0 && rows > own.1,
                "the picture was left at its own size {own:?}: {cols}x{rows}"
            );
            assert_capped((cols, rows), own, room);
            // In the room it nearly fills, the picture stops just short of the
            // end of it rather than at some middle distance.
            if room == (120, 40) {
                assert!(rows + own.1 > room.1, "the picture stopped short of the room at {rows} rows");
            }
        }
    }

    #[test]
    fn no_picture_is_drawn_bigger_than_the_room_it_is_drawn_in() {
        // Whatever rule chose its size, nothing may overhang the column: a
        // picture too big for the room is capped by it — every row there is
        // and not one more — with its shape kept, a mark in a room too small
        // for it is capped too, and a plate wider than the room is capped
        // before the centre offset could push it off the left edge. Checked
        // rather than assumed.
        let cell = CellSize { width: 10, height: 31 };
        let (cols, rows) = measure((1200, 1600), 120, 40, Role::Keep, cell);
        assert_eq!(rows, 40, "the picture does not fill the room it has");
        // 1200x1600 capped by the height: 40 rows of 31 pixels, scaled in
        // proportion, is 93 columns of 10.
        let by_shape = 40.0f32 * (1200.0 / 1600.0) * (31.0 / 10.0);
        assert!((cols as f32 - by_shape).abs() <= 1.0, "the picture lost its shape at {cols} columns");
        assert_capped((cols, rows), own_size((1200, 1600), cell), (120, 40));

        // The two small rooms, under both rules: a mark cut down to the 4x1
        // room it was drawn in, and a plate that would need 80 columns in an
        // 11-cell room — the slack the centre offset is half of can never be
        // negative, so the room holds once the picture is centred as well.
        for (pixels, room, cell) in [((48, 48), (4, 1), cell), ((640, 480), (11, 6), CellSize::default())] {
            for role in [Role::Fill, Role::Keep] {
                let (cols, rows) = measure(pixels, room.0, room.1, role, cell);
                assert!(cols >= 1 && rows >= 1, "{role:?} drew {cols}x{rows}");
                assert_capped((cols, rows), own_size(pixels, cell), room);
                let indent = crate::layout::centred_offset(room.0, cols);
                assert!(indent + cols <= room.0, "{role:?} leaves the room once centred: {indent} + {cols} > {}", room.0);
            }
        }
    }
}
