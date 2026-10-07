//! Showing images in the terminal.
//!
//! The reader must work in every terminal the user might pick, and through tmux.
//! That rules out relying on a graphics protocol: Ghostty speaks the Kitty
//! protocol, foot speaks Sixel, alacritty speaks neither, and tmux breaks both
//! because it manages the screen contents itself.
//!
//! What works everywhere is the half block. Each cell shows `▀` with the
//! foreground painted for the upper pixel and the background for the lower one,
//! so one cell carries two pixels. The result is coarse but universal, and it
//! survives scrolling and redrawing like any other text.

pub mod detect;
mod kitty;
mod sixel;

pub use detect::Backend;

use anyhow::{Context, Result};
use image::codecs::png::{CompressionType, PngEncoder};
use image::imageops::FilterType;
use image::{DynamicImage, ExtendedColorType, GenericImageView, ImageEncoder};

/// A single cell of a rendered image: two stacked pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub upper: (u8, u8, u8),
    pub lower: (u8, u8, u8),
}

/// A picture ready to be drawn, in whichever form the terminal takes.
#[derive(Debug, Clone)]
pub struct Rendered {
    /// Cells the picture occupies. The layout reserves this many lines either
    /// way, so text flows around the picture the same in every backend.
    /// The view itself only needs the height — the escape carries the width
    /// and the cells are their own measure — but the tests that hold
    /// `measure` and `render` together ask for both.
    #[cfg_attr(not(test), allow(dead_code))]
    cols: u16,
    rows: u16,
    payload: Payload,
}

#[derive(Debug, Clone)]
enum Payload {
    /// Two pixels per cell, drawn as ordinary text.
    HalfBlocks(Vec<Vec<Cell>>),
    /// A ready-made escape sequence, written past the text buffer.
    Escape(String),
}

impl Rendered {
    pub fn height(&self) -> usize {
        self.rows as usize
    }

    /// Cells across. Only the tests ask: the view needs the height, and the
    /// escape sequence carries the width the terminal paints.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn width(&self) -> usize {
        self.cols as usize
    }

    /// The cell rows, for the half block backend.
    pub fn cells(&self) -> Option<&[Vec<Cell>]> {
        match &self.payload {
            Payload::HalfBlocks(rows) => Some(rows),
            Payload::Escape(_) => None,
        }
    }

    /// The escape sequence that places this picture at the cursor.
    pub fn escape(&self) -> Option<&str> {
        match &self.payload {
            Payload::Escape(text) => Some(text),
            Payload::HalfBlocks(_) => None,
        }
    }
}

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
/// `MARK_COLS` cells, is a mark and is drawn at its own size. A line of text
/// and a few glyphs across: the size a footnote's marker has when it is a
/// character instead of a picture.
const MARK_ROWS: u16 = 3;
const MARK_COLS: u16 = 20;

/// Decodes image bytes and prepares them for the given backend.
///
/// `max_cols` and `max_rows` are the room: the picture may fill them but never
/// exceed them. `role` says which rule sizes the picture: the room for a cover
/// or a page, and in the flow of text either the mark's own size or the room
/// within `MAX_UPSCALE`. `id` distinguishes pictures for backends that address
/// them, which is how a stale picture gets removed before the next draw.
pub fn render(
    bytes: &[u8],
    max_cols: u16,
    max_rows: u16,
    role: Role,
    backend: Backend,
    id: u32,
    cell: CellSize,
) -> Result<Rendered> {
    let decoded = decode(bytes)?;
    Ok(match backend {
        Backend::HalfBlocks => fit(&decoded, max_cols, max_rows, role, cell),
        Backend::Kitty | Backend::Sixel => {
            fit_pixels(&decoded, bytes, max_cols, max_rows, role, backend, id, cell)
        }
    })
}

/// Turns the bytes of a picture into pixels.
///
/// The decoder sniffs the format from the bytes, so PNG, JPEG, GIF and WebP all
/// arrive here. Keep it the one place that knows: this is where a format needing
/// a decoder of its own would plug in, and a format nothing can read has to fail
/// the same way a corrupt JPEG does rather than draw something wrong.
fn decode(bytes: &[u8]) -> Result<DynamicImage> {
    image::load_from_memory(bytes).context("cannot decode image")
}

/// Pixel size of an image, read from its header alone.
///
/// A chapter of rendered formulas holds hundreds of pictures, and the layout
/// needs the height of each one. Decoding them all to find out costs seconds,
/// so the size is taken from the header, which is a few bytes in.
pub fn dimensions(bytes: &[u8]) -> Result<(u32, u32)> {
    // Whatever `decode` can decode, this has to measure, or the layout reserves
    // no lines for a picture and the picture is skipped without a word.
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .context("cannot read image header")?
        .into_dimensions()
        .context("cannot read image size")
}

/// The cell box a picture will occupy, from its pixel size alone.
///
/// How a picture is sized follows from what it is in the book, and there are
/// two answers — the classification every serious reader gives its pictures:
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
///   a 48x48 footnote mark stays the mark its publisher drew, where
///   magnifying it turned it into a blur holding the whole screen. Anything
///   larger is magnified within `MAX_UPSCALE` to fill the room — the rule a
///   body picture had before the roles arrived, and the one that shows a
///   diagram at the size of the page instead of at the size of a stamp.
///
/// The room is the cap on every rule here, so nothing ever overflows the view.
///
/// Shared with the two `fit` functions below, so a measured height and a
/// rendered one cannot drift apart.
pub fn measure(
    (width, height): (u32, u32),
    max_cols: u16,
    max_rows: u16,
    role: Role,
    backend: Backend,
    cell: CellSize,
) -> (u16, u16) {
    if width == 0 || height == 0 {
        return (0, 0);
    }
    let cell_w = cell.width.max(1) as f32;
    let cell_h = cell.height.max(1) as f32;
    // The picture's own size in cells: pixels divided by one cell, each side
    // rounded up, because a cell is the smallest thing a screen can hold.
    let natural_cols = (width as f32 / cell_w).ceil().max(1.0) as u16;
    let natural_rows = (height as f32 / cell_h).ceil().max(1.0) as u16;
    // The rows its own width needs with the shape kept. Both backends arrive
    // at this count for a picture shown at its own size, so it is what a whole
    // step up is a multiple of.
    let aspect = cell_h / cell_w;
    let own_rows = ((height as f32 / width as f32) * natural_cols as f32 / aspect)
        .round()
        .max(1.0)
        .min(natural_rows as f32) as u16;

    // A mark: no taller than `MARK_ROWS` rows of text and no wider than
    // `MARK_COLS` cells at the picture's own size — a line of text and a few
    // glyphs across, which is the size a footnote's marker has when it is a
    // character instead of a picture. Only a picture in the flow of text is
    // tested for it: a cover or a page is a screen whatever its pixels, so it
    // fills the room without being asked whether it happens to be small.
    let mark = natural_rows <= MARK_ROWS && natural_cols <= MARK_COLS;

    let (box_cols, box_rows) = match role {
        // A mark keeps its own size, whatever room there is: magnified, the
        // `注` a publisher drew 48 pixels across stops being the glyph it was
        // and becomes a blur holding the room.
        Role::Keep if mark => (
            natural_cols.min(max_cols).max(1),
            natural_rows.min(max_rows).max(1),
        ),
        // A picture drawn bigger than it is: the cover, the page, and an
        // illustration in the body of the book, which fills the room as it
        // did before the roles were drawn. Never more than this rule's own
        // magnification times the picture's own size in cells — the room may
        // cap a picture further, and may ask for less than the cap when the
        // picture is already big.
        Role::Fill | Role::Keep => {
            let upscale = match role {
                Role::Fill => MAGNIFY,
                Role::Keep => MAX_UPSCALE,
            };
            let cap_cols = natural_cols.saturating_mul(upscale).max(1);
            let cap_rows = natural_rows.saturating_mul(upscale).max(1);
            match backend {
                // Half blocks enlarge in whole steps of the picture's own cell
                // size — twice, three times, and nothing in between. A fractional
                // step makes each drawn pixel cover a different fraction of a
                // cell, which is the shimmer block-drawn pixels show when the
                // factor is not whole; an integer step keeps every source pixel
                // on a whole block of cells, so they stay square. The step is
                // also capped at `upscale`, which is where the cap bites here:
                // each step is the picture's own size.
                Backend::HalfBlocks => {
                    let steps = (max_cols / natural_cols)
                        .min(max_rows / own_rows)
                        .min(upscale)
                        .max(1);
                    let stepped = (natural_cols * steps, own_rows * steps);
                    if stepped.0 <= max_cols && stepped.1 <= max_rows {
                        return stepped;
                    }
                    // Not even its own size fits the room, so there is nothing
                    // to enlarge: it is capped like any picture too big to fit.
                    (
                        natural_cols.min(max_cols).max(1),
                        natural_rows.min(max_rows).max(1),
                    )
                }
                // A pixel protocol scales the picture to whatever box it is
                // given, so the box is the room within the cap: fill it,
                // aspect preserved.
                _ => (max_cols.min(cap_cols).max(1), max_rows.min(cap_rows).max(1)),
            }
        }
    };
    let (max_cols, max_rows) = (box_cols as f32, box_rows as f32);
    let (cols, rows) = match backend {
        Backend::HalfBlocks => {
            // One column is one pixel wide, one row is two tall, and a cell is
            // as tall as the terminal says: with a wide, short cell the picture
            // needs more rows for the same width, or it comes out stretched.
            let cols = max_cols;
            let rows_from_width = (height as f32 / width as f32) * cols / aspect;
            let rows = rows_from_width.round().max(1.0).min(max_rows);
            // Recompute the width so a capped height does not stretch the picture.
            let cols = (rows * aspect * width as f32 / height as f32)
                .round()
                .max(1.0)
                .min(cols);
            (cols, rows)
        }
        Backend::Kitty | Backend::Sixel => {
            let by_width = max_cols;
            let rows_needed = (height as f32 * (by_width * cell_w) / width as f32) / cell_h;
            if rows_needed <= max_rows {
                (by_width, rows_needed.round().max(1.0))
            } else {
                let rows = max_rows;
                let cols = (width as f32 * (rows * cell_h) / height as f32) / cell_w;
                (cols.round().max(1.0).min(by_width), rows)
            }
        }
    };
    (cols as u16, rows as u16)
}

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

/// Whether these bytes are already a PNG, which the Kitty protocol takes as
/// they are.
fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a])
}

/// Re-encodes decoded pixels as PNG.
///
/// The Kitty protocol understands PNG, raw RGB and raw RGBA, and nothing else.
/// A book hands us whichever format its publisher chose, and JPEG is the common
/// case, so the pixels decoded above are encoded back to PNG rather than the
/// original file travelling on.
///
/// These escapes are rebuilt on every redraw, so the encoding aims at the
/// smallest PNG it can: RGB while every pixel is opaque, and the compressor set
/// to work harder than it would by default. A photograph does not compress well
/// without loss either way, but the difference between the two settings is
/// roughly a third of the bytes on the wire.
fn encode_as_png(decoded: &DynamicImage) -> Vec<u8> {
    let mut out = Vec::new();
    let encoder = PngEncoder::new_with_quality(
        &mut out,
        CompressionType::Best,
        image::codecs::png::FilterType::Adaptive,
    );

    let written = if decoded.color().has_alpha() {
        let rgba = decoded.to_rgba8();
        encoder.write_image(
            rgba.as_raw(),
            rgba.width(),
            rgba.height(),
            ExtendedColorType::Rgba8,
        )
    } else {
        let rgb = decoded.to_rgb8();
        encoder.write_image(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            ExtendedColorType::Rgb8,
        )
    };

    written.expect("writing a PNG into memory cannot fail");
    out
}

/// Prepares a picture for a pixel protocol, sized to a whole number of cells.
fn fit_pixels(
    decoded: &DynamicImage,
    original: &[u8],
    max_cols: u16,
    max_rows: u16,
    role: Role,
    backend: Backend,
    id: u32,
    cell: CellSize,
) -> Rendered {
    let (cols, rows) = measure(
        decoded.dimensions(),
        max_cols,
        max_rows,
        role,
        backend,
        cell,
    );
    if cols == 0 || rows == 0 {
        return Rendered {
            cols: 0,
            rows: 0,
            payload: Payload::Escape(String::new()),
        };
    }

    let escape = match backend {
        // Kitty scales the picture itself, which keeps every pixel, but it only
        // takes PNG: the escape announces PNG in `f=100`, so a JPEG sent under
        // that header is a lie the terminal is free to drop, and every one of
        // them does, silently. A PNG travels on unchanged; anything else is
        // re-encoded from the pixels already decoded here.
        Backend::Kitty => {
            if is_png(original) {
                kitty::encode_png(original, cols, rows, id)
            } else {
                kitty::encode_png(&encode_as_png(decoded), cols, rows, id)
            }
        }
        // Sixel carries no scaling, so the pixels are resized to the cell box.
        Backend::Sixel => {
            let pixel_width = cols as u32 * cell.width.max(1) as u32;
            let pixel_height = rows as u32 * cell.height.max(1) as u32;
            let scaled = decoded
                .resize_exact(
                    pixel_width.max(1),
                    pixel_height.max(1),
                    FilterType::Triangle,
                )
                .to_rgba8();
            sixel::encode(&scaled)
        }
        Backend::HalfBlocks => String::new(),
    };

    Rendered {
        cols,
        rows,
        payload: Payload::Escape(escape),
    }
}

/// Removes every picture a pixel backend has placed. Called before a redraw,
/// because pictures live outside the text buffer and would otherwise stay put.
pub fn clear_all(backend: Backend) -> Option<String> {
    match backend {
        Backend::Kitty => Some(kitty::delete_all()),
        // Sixel pictures are part of the screen contents and vanish with it.
        Backend::Sixel | Backend::HalfBlocks => None,
    }
}

fn fit(
    decoded: &DynamicImage,
    max_cols: u16,
    max_rows: u16,
    role: Role,
    cell: CellSize,
) -> Rendered {
    let (cols, rows) = measure(
        decoded.dimensions(),
        max_cols,
        max_rows,
        role,
        Backend::HalfBlocks,
        cell,
    );
    if cols == 0 || rows == 0 {
        return Rendered {
            cols: 0,
            rows: 0,
            payload: Payload::HalfBlocks(Vec::new()),
        };
    }

    // Target size in pixels: one column is one pixel wide, one row is two tall.
    let pixel_width = cols as u32;
    let pixel_height = rows as u32 * 2;
    let scaled = decoded
        .resize_exact(pixel_width, pixel_height, FilterType::Triangle)
        .to_rgba8();

    let mut out = Vec::with_capacity(rows as usize);
    for row in 0..rows as u32 {
        let mut cells = Vec::with_capacity(pixel_width as usize);
        for column in 0..pixel_width {
            let upper = scaled.get_pixel(column, row * 2);
            let lower_y = (row * 2 + 1).min(pixel_height.saturating_sub(1));
            let lower = scaled.get_pixel(column, lower_y);
            cells.push(Cell {
                upper: flatten(upper.0),
                lower: flatten(lower.0),
            });
        }
        out.push(cells);
    }
    Rendered {
        cols: pixel_width as u16,
        rows: out.len() as u16,
        payload: Payload::HalfBlocks(out),
    }
}

/// Composites a pixel onto white.
///
/// Many book illustrations are line drawings on transparency. Left alone they
/// would come out as black on black in a dark terminal, so transparency is
/// resolved against white, the colour the author assumed.
pub(crate) fn flatten([r, g, b, a]: [u8; 4]) -> (u8, u8, u8) {
    if a == 255 {
        return (r, g, b);
    }
    let alpha = a as f32 / 255.0;
    let over = |channel: u8| -> u8 {
        (channel as f32 * alpha + 255.0 * (1.0 - alpha))
            .round()
            .clamp(0.0, 255.0) as u8
    };
    (over(r), over(g), over(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn solid(width: u32, height: u32, color: [u8; 4]) -> DynamicImage {
        let mut buffer = RgbaImage::new(width, height);
        for pixel in buffer.pixels_mut() {
            *pixel = Rgba(color);
        }
        DynamicImage::ImageRgba8(buffer)
    }

    #[test]
    fn fits_the_room_and_keeps_its_shape() {
        // Whatever room it is given, `fit` answers with a picture inside it:
        // no wider than the columns, no taller than the rows, aspect kept — a
        // tall picture stays narrow rather than stretching to the width.
        let cell = CellSize {
            width: 1,
            height: 2,
        };
        let rendered = fit(
            &solid(100, 100, [10, 20, 30, 255]),
            40,
            100,
            Role::Fill,
            cell,
        );
        assert!(rendered.width() <= 40);
        assert!(rendered.height() >= 1);
        // A square picture takes the cells its shape needs: a cell one wide
        // and two tall makes it half as many rows as columns.
        let square = fit(&solid(100, 100, [0, 0, 0, 255]), 40, 100, Role::Fill, cell);
        let expected = square.width() as f32 * cell.width as f32 / cell.height as f32;
        assert!(
            (square.height() as f32 - expected).abs() <= 1.0,
            "{} rows for {} columns",
            square.height(),
            square.width()
        );
        let tall = fit(
            &solid(100, 1000, [0, 0, 0, 255]),
            80,
            10,
            Role::Fill,
            cell,
        );
        assert!(tall.height() <= 10, "{} rows", tall.height());
        assert!(
            tall.width() < 80,
            "{} columns is too wide for 10 rows",
            tall.width()
        );
    }

    #[test]
    fn the_pixels_come_through_as_they_are_written() {
        let rendered = fit(
            &solid(10, 10, [200, 100, 50, 255]),
            10,
            10,
            Role::Fill,
            CellSize::default(),
        );
        let cell = rendered.cells().unwrap()[0][0];
        assert_eq!(cell.upper, (200, 100, 50));
        assert_eq!(cell.lower, (200, 100, 50));

        // Transparency is resolved against white, the colour the author of a
        // line drawing assumed, not against the terminal's black.
        assert_eq!(flatten([0, 0, 0, 255]), (0, 0, 0));
        assert_eq!(flatten([0, 0, 0, 0]), (255, 255, 255));
        let half = flatten([0, 0, 0, 128]);
        assert!(half.0 > 100 && half.0 < 160, "{half:?}");
    }

    /// A PNG of the given size, so `render` can be driven the way the reader
    /// drives it: from bytes.
    fn encoded(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = std::io::Cursor::new(Vec::new());
        solid(width, height, [0, 0, 0, 255])
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

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

    fn encoded_as(width: u32, height: u32, format: image::ImageFormat) -> Vec<u8> {
        let mut bytes = std::io::Cursor::new(Vec::new());
        solid(width, height, [200, 100, 50, 255])
            .write_to(&mut bytes, format)
            .unwrap();
        bytes.into_inner()
    }

    /// What the escape carries, decoded. Chunks are joined first: a payload
    /// longer than 4096 base64 characters arrives in several of them.
    fn carried(escape: &str) -> Vec<u8> {
        use base64::Engine;
        let mut payload = String::new();
        let mut rest = escape;
        while let Some(start) = rest.find("\x1b_G") {
            let after = &rest[start + 3..];
            let (_control, body) = after.split_once(';').expect("an escape carries payload");
            let (data, _) = body.split_once("\x1b\\").expect("an escape ends with ST");
            payload.push_str(data);
            rest = body;
        }
        base64::engine::general_purpose::STANDARD
            .decode(payload)
            .expect("the payload is base64")
    }

    fn kitty_escape(bytes: &[u8]) -> String {
        let rendered = render(
            bytes,
            40,
            20,
            Role::Fill,
            Backend::Kitty,
            1,
            CellSize::default(),
        )
        .unwrap();
        match rendered.payload {
            Payload::Escape(escape) => escape,
            Payload::HalfBlocks(_) => panic!("the kitty backend produced cells instead"),
        }
    }

    #[test]
    fn a_picture_travels_in_the_format_it_announces() {
        // The kitty escape announces PNG in `f=100`, and every terminal trusts
        // that: a JPEG sent under the header was dropped without a word, and a
        // picture that is not a PNG is the common case in a real book.
        let jpeg = encoded_as(40, 30, image::ImageFormat::Jpeg);
        assert_eq!(&jpeg[..3], &[0xff, 0xd8, 0xff], "the fixture is not a JPEG");
        let escape = kitty_escape(&jpeg);
        assert!(
            escape.contains("f=100"),
            "the escape no longer announces PNG"
        );
        let payload = carried(&escape);
        assert!(
            payload.starts_with(b"\x89PNG\r\n\x1a\n"),
            "a JPEG travelled under a PNG header"
        );

        // And the point of sending a PNG as it is: no quality lost, no work.
        let png = encoded(20, 20);
        assert_eq!(carried(&kitty_escape(&png)), png);
    }

    #[test]
    fn something_that_is_not_a_picture_is_an_error_not_a_panic() {
        for backend in [Backend::HalfBlocks, Backend::Kitty, Backend::Sixel] {
            assert!(
                render(
                    b"not a picture",
                    40,
                    20,
                    Role::Fill,
                    backend,
                    1,
                    CellSize::default()
                )
                .is_err()
            );
        }
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
            for backend in [Backend::HalfBlocks, Backend::Kitty, Backend::Sixel] {
                assert_eq!(
                    measure((100, 100), 0, 0, role, backend, cell),
                    (1, 1),
                    "{role:?} {backend:?}"
                );
                let (cols, rows) = measure((1, 1), 120, 40, role, backend, cell);
                assert!(
                    cols >= 1 && rows >= 1,
                    "{role:?} {backend:?} made {cols}x{rows}"
                );
            }
        }
    }

    #[test]
    fn a_footnote_mark_keeps_its_own_size() {
        // 48x48 pixels is a `注` beside a footnote — the size of a footnote's
        // glyph — and its own size is what it keeps, whatever room there is:
        // five cells across by two down at a 10x31 cell, six by three at an
        // 8x16 one, in every backend. A mark is never magnified, and this is
        // what that means for the smallest thing a book draws — at the 8x16
        // cell it comes to three rows, the tallest a mark may be and still be
        // a mark.
        for backend in [Backend::HalfBlocks, Backend::Kitty, Backend::Sixel] {
            assert_eq!(
                measure(
                    (48, 48),
                    120,
                    40,
                    Role::Keep,
                    backend,
                    CellSize {
                        width: 10,
                        height: 31,
                    }
                ),
                (5, 2),
                "{backend:?} at a 10x31 cell"
            );
            assert_eq!(
                measure(
                    (48, 48),
                    120,
                    40,
                    Role::Keep,
                    backend,
                    CellSize {
                        width: 8,
                        height: 16,
                    }
                ),
                (6, 3),
                "{backend:?} at an 8x16 cell"
            );
        }
    }

    #[test]
    fn a_cover_fills_the_room_and_is_capped_at_four_times_its_own_size() {
        // A cover is a screen of its own: aspect kept, scaled up to fill the
        // room — the one case where magnifying is the whole point — but never
        // past four times its own size, or a book that calls a pixel the cover
        // would take the whole screen. The cap does not stop a real cover from
        // filling what four times its own size still covers.
        let cell = CellSize {
            width: 10,
            height: 20,
        };
        let own = own_size((150, 200), cell);
        for backend in [Backend::HalfBlocks, Backend::Kitty, Backend::Sixel] {
            let (cols, rows) = measure((150, 200), 120, 40, Role::Fill, backend, cell);
            assert_eq!(rows, 40, "{backend:?} does not use the whole height");
            assert!(
                cols <= 120,
                "{backend:?} stays inside the room at {cols} columns"
            );
            assert!(
                cols > own.0 && rows > own.1,
                "{backend:?} drew the cover at its own size {own:?} instead of filling: {cols}x{rows}"
            );
            assert!(
                cols <= own.0 * 4 && rows <= own.1 * 4,
                "{backend:?} passed four times the cover's own size {own:?}: {cols}x{rows}"
            );
        }

        // The cap, at the other end of the scale: `properties="cover-image"`
        // on a 1x1 pixel, or one `rendition:layout` line on an ordinary book.
        for backend in [Backend::HalfBlocks, Backend::Kitty, Backend::Sixel] {
            let (cols, rows) = measure((1, 1), 80, 20, Role::Fill, backend, CellSize::default());
            assert!(
                cols >= 1 && rows >= 1 && cols <= 4 && rows <= 4,
                "{backend:?} drew a 1x1 pixel as {cols}x{rows} cells"
            );
        }

        // And a mark-sized picture marked a cover is a cover: magnified like
        // one, up to the same four times, wherever the room happens to end.
        let cell = CellSize {
            width: 10,
            height: 31,
        };
        let own = own_size((48, 48), cell);
        for backend in [Backend::HalfBlocks, Backend::Kitty, Backend::Sixel] {
            let (cols, rows) = measure((48, 48), 120, 40, Role::Fill, backend, cell);
            assert!(
                cols > own.0 && rows > own.1,
                "{backend:?} drew the cover at its own size {own:?}: {cols}x{rows}"
            );
            assert!(
                cols <= own.0 * 4 && rows <= own.1 * 4,
                "{backend:?} passed four times the cover's own size {own:?}: {cols}x{rows}"
            );
            assert!(cols <= 120 && rows <= 40, "{backend:?} left the room");
        }
    }

    #[test]
    fn a_body_picture_is_magnified_into_the_room_and_stops_at_four_times() {
        // A diagram in the flow of text has one rule: grow into the room —
        // magnified up to four times its own size, capped by the room, in
        // every backend, and stopped by the four rather than by the room when
        // the room would take a dozen times the picture. The block backend
        // enlarges in whole steps of the picture's own size, so it lands on
        // the last step that still fits and not one whole step short of the
        // room.
        let cell = CellSize {
            width: 10,
            height: 31,
        };
        let own = own_size((300, 400), cell);
        for backend in [Backend::HalfBlocks, Backend::Kitty, Backend::Sixel] {
            let (cols, rows) = measure((300, 400), 120, 40, Role::Keep, backend, cell);
            assert!(
                cols > own.0 && rows > own.1,
                "{backend:?} left the picture at its own size {own:?}: {cols}x{rows}"
            );
            assert!(
                cols <= own.0 * 4 && rows <= own.1 * 4,
                "{backend:?} magnified past four times the picture's own size {own:?}: {cols}x{rows}"
            );
            assert!(
                cols <= 120 && rows <= 40,
                "{backend:?} left the room: {cols}x{rows}"
            );
            assert!(
                rows + own.1 > 40,
                "{backend:?} stopped short of the room at {rows} rows"
            );
        }

        // The same picture in a room that would take a dozen times its size.
        let cell = CellSize {
            width: 8,
            height: 16,
        };
        let own = own_size((300, 400), cell);
        assert_eq!(own, (38, 25), "the picture's own size in cells");
        for backend in [Backend::HalfBlocks, Backend::Kitty, Backend::Sixel] {
            let (cols, rows) = measure((300, 400), 400, 200, Role::Keep, backend, cell);
            assert!(
                cols > own.0 && rows > own.1,
                "{backend:?} left the picture at its own size {own:?}: {cols}x{rows}"
            );
            assert!(
                cols <= own.0 * 4 && rows <= own.1 * 4,
                "{backend:?} magnified past four times the picture's own size {own:?}: {cols}x{rows}"
            );
            assert!(
                cols <= 400 && rows <= 200,
                "{backend:?} left the room: {cols}x{rows}"
            );
        }
    }

    #[test]
    fn no_picture_is_drawn_bigger_than_the_room_it_is_drawn_in() {
        // Whatever rule chose its size, nothing may overhang the column: a
        // picture too big for the room is capped by it — every row there is
        // and not one more — with its shape kept, a mark in a room too small
        // for it is capped too, and a plate wider than the room is capped
        // before the centre offset could push it off the left edge. Checked
        // rather than assumed, in every backend.
        let cell = CellSize {
            width: 10,
            height: 31,
        };
        for backend in [Backend::HalfBlocks, Backend::Kitty, Backend::Sixel] {
            let (cols, rows) = measure((1200, 1600), 120, 40, Role::Keep, backend, cell);
            assert_eq!(rows, 40, "{backend:?} does not fill the room it has");
            assert!(
                cols <= 120,
                "{backend:?} exceeds the room at {cols} columns"
            );
            // 1200x1600 capped by the height: 40 rows of 31 pixels, scaled in
            // proportion, is 93 columns of 10.
            let by_shape = 40.0f32 * (1200.0 / 1600.0) * (31.0 / 10.0);
            assert!(
                (cols as f32 - by_shape).abs() <= 1.0,
                "{backend:?} lost its shape at {cols} columns"
            );

            let (cols, rows) = measure((48, 48), 4, 1, Role::Keep, backend, cell);
            assert!(
                cols >= 1 && rows >= 1 && cols <= 4 && rows <= 1,
                "{backend:?} drew a mark at {cols}x{rows} in a 4x1 room"
            );
        }

        // A plate that would need 80 columns in an 11-cell room, under both
        // rules: the slack the centre offset is half of can never be negative.
        for role in [Role::Fill, Role::Keep] {
            for backend in [Backend::HalfBlocks, Backend::Kitty, Backend::Sixel] {
                let (cols, rows) = measure((640, 480), 11, 6, role, backend, CellSize::default());
                assert!(
                    cols <= 11 && rows <= 6,
                    "{role:?} {backend:?} drew {cols}x{rows} in an 11x6 room"
                );
                let indent = crate::layout::centred_offset(11, cols);
                assert!(
                    indent + cols <= 11,
                    "{role:?} {backend:?} leaves the room once centred: {indent} + {cols} > 11"
                );
            }
        }
    }

    #[test]
    fn a_measured_picture_comes_out_exactly_that_tall() {
        // The layout reserves rows from `measure` and the view paints what
        // `render` produced. They must agree, or text lands on top of a picture.
        // Both an 8x16 cell — which is `CellSize::default()`, so a `fit` that
        // quietly went back to the default would still agree with itself — and a
        // 10x31 one, where it would not.
        let bytes = encoded(640, 480);
        for cell in [
            CellSize {
                width: 8,
                height: 16,
            },
            CellSize {
                width: 10,
                height: 31,
            },
        ] {
            for role in [Role::Fill, Role::Keep] {
                for backend in [Backend::HalfBlocks, Backend::Kitty, Backend::Sixel] {
                    let measured = measure(dimensions(&bytes).unwrap(), 40, 12, role, backend, cell);
                    let rendered = render(&bytes, 40, 12, role, backend, 1, cell).unwrap();
                    assert_eq!(
                        (rendered.width() as u16, rendered.height() as u16),
                        measured,
                        "{role:?} {backend:?} with a {}x{} cell",
                        cell.width,
                        cell.height
                    );
                    // Centred in the same room: the layout indents from
                    // the measured width, and what it draws is the
                    // picture `render` produced — so the drawn picture
                    // must land inside the room with its two sides even
                    // to within a cell, as it must for the rows above.
                    let room = 40u16;
                    let indent = crate::layout::centred_offset(room, measured.0);
                    let left = indent;
                    let right = room
                        .checked_sub(indent + rendered.width() as u16)
                        .expect("the centred picture fits the room");
                    assert!(
                        left == right || left + 1 == right,
                        "the picture stands centred: {left} cells left, {right} right, {role:?} {backend:?}"
                    );
                }
            }
        }
    }
}
