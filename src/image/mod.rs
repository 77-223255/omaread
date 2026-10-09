//! Showing images in the terminal.
//!
//! The reader must work in every terminal the user might pick, and through tmux.
//! That rules out relying on a graphics protocol: Ghostty speaks the Kitty
//! protocol, foot speaks Sixel, alacritty speaks neither, and tmux breaks both
//! because it manages the screen contents itself.
//!
//! What works everywhere is the block: a character cell is split into four
//! quadrants, each one pixel of that patch of the picture, painted in one of
//! two colours — the foreground for the bright quadrants, the background for
//! the dark ones. The result is coarse but universal, and it survives
//! scrolling and redrawing like any other text.

pub mod detect;
// Public because the session writes the placements, and a cropped one is
// built here from the payload this module cached.
pub mod kitty;
mod quad;
mod sixel;
mod size;

pub use detect::Backend;
pub use size::{CellSize, Reason, Role, is_mark, measure};

use quad::fit;

use anyhow::{Context, Result};
use image::codecs::png::{CompressionType, PngEncoder};
use image::imageops::FilterType;
use image::{DynamicImage, ExtendedColorType, GenericImageView, ImageEncoder};

/// A single cell of a rendered image: the glyph that draws its four quadrants
/// and the two colours it is painted in.
///
/// The bright quadrants take `fg` and the dark ones `bg`. A cell whose pixels
/// cannot be told apart fills with one colour, so it reads as a patch of that
/// colour rather than as noise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: (u8, u8, u8),
    pub bg: (u8, u8, u8),
}

/// A picture ready to be drawn, in whichever form the terminal takes.
#[derive(Debug, Clone)]
pub struct Rendered {
    /// Cells the picture occupies. The layout reserves this many lines either
    /// way, so text flows around the picture the same in every backend.
    /// The view itself only needs the height — the escape carries the width
    /// and the cells are their own measure — but the tests that hold
    /// `measure` and `render` together ask for both.
    cols: u16,
    rows: u16,
    payload: Payload,
    /// What a frame writes this picture from when it is pixels: kitty's id
    /// and source rectangle — the payload itself travelled once, when this
    /// was rendered — or Sixel's header and bands. `None` for the block
    /// backend, whose cells clip like any other text.
    pixel: Option<PixelInfo>,
}

/// The parts a frame writes a pixel picture from, beyond the escape the
/// payload holds.
#[derive(Debug, Clone)]
pub enum PixelInfo {
    /// The kitty protocol: the picture's pixel size — the rectangle a crop
    /// names its source from — its id, and the PNG the placement carries.
    /// Keeping the payload lets a cropped frame rebuild the escape without
    /// decoding anything again.
    Kitty {
        source: (u32, u32),
        id: u32,
        png: Vec<u8>,
    },
    /// Sixel's palette header and its bands, so a crop takes whole bands —
    /// a band cut in half would lose the colours drawn after the cut.
    Sixel {
        source: (u32, u32),
        header: String,
        bands: Vec<String>,
    },
}

#[derive(Debug, Clone)]
enum Payload {
    /// A block glyph per cell, drawn as ordinary text.
    Cells(Vec<Vec<Cell>>),
    /// A ready-made escape sequence, written past the text buffer.
    Escape(String),
}

impl Rendered {
    pub fn height(&self) -> usize {
        self.rows as usize
    }

    /// Cells across. The view needs the height and the escape carries the
    /// width, but a cropped write rebuilds the header with the width in it,
    /// and the tests that hold `measure` and `render` together ask for both.
    pub fn width(&self) -> usize {
        self.cols as usize
    }

    /// The cell rows, for the block backend.
    pub fn cells(&self) -> Option<&[Vec<Cell>]> {
        match &self.payload {
            Payload::Cells(rows) => Some(rows),
            Payload::Escape(_) => None,
        }
    }

    /// The escape that draws this picture whole: kitty's placement of the
    /// full picture, or the whole Sixel picture, written at the cursor.
    pub fn escape(&self) -> Option<&str> {
        match &self.payload {
            Payload::Escape(text) => Some(text),
            Payload::Cells(_) => None,
        }
    }

    /// Where a frame gets a placement's id and source rectangle — or a
    /// Sixel crop its bands — when the picture is drawn as pixels.
    pub fn pixel(&self) -> Option<&PixelInfo> {
        self.pixel.as_ref()
    }

    /// Whether this picture is pixels past the text buffer: placed, cropped
    /// and written after the draw, where the block backend has cells that
    /// clip like any other text.
    pub fn is_pixel(&self) -> bool {
        self.pixel.is_some()
    }
}

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
        Backend::Quad => fit(&decoded, max_cols, max_rows, role, cell),
        Backend::Kitty | Backend::Sixel => {
            fit_pixels(&decoded, max_cols, max_rows, role, backend, id, cell)
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


/// Re-encodes decoded pixels as PNG.
///
/// The Kitty protocol understands PNG, raw RGB and raw RGBA, and nothing else.
/// A book hands us whichever format its publisher chose, and JPEG is the common
/// case, so the pixels decoded above are encoded back to PNG rather than the
/// original file travelling on.
///
/// The pixels arrive already sized to their cell box — there is no zoom that
/// could ever show the ones past it — and the payload travels once rather than
/// per frame, so what the compressor owes is a frame that is not held up: the
/// cheap setting runs in milliseconds where the thorough one cost hundreds on
/// the UI thread, for bytes the terminal never sees again.
fn encode_as_png(decoded: &DynamicImage) -> Vec<u8> {
    let mut out = Vec::new();
    let encoder = PngEncoder::new_with_quality(
        &mut out,
        CompressionType::Fast,
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
///
/// The arguments are the picture, the room it was measured for, and the
/// protocol that will draw it; bundling them would hide more than it saves.
fn fit_pixels(
    decoded: &DynamicImage,
    max_cols: u16,
    max_rows: u16,
    role: Role,
    backend: Backend,
    id: u32,
    cell: CellSize,
) -> Rendered {
    let (cols, rows) = measure(decoded.dimensions(), max_cols, max_rows, role, cell);
    if cols == 0 || rows == 0 {
        return Rendered {
            cols: 0,
            rows: 0,
            payload: Payload::Escape(String::new()),
            // Nothing to place: an escape no bytes wide paints nothing, and a
            // crop of it would say the same.
            pixel: None,
        };
    }

    let (escape, pixel) = match backend {
        // Kitty only takes PNG, so a JPEG is re-encoded from the pixels
        // decoded above — under a header that claims PNG, because a JPEG sent
        // under `f=100` is a lie every terminal drops, silently. The terminal
        // would scale the picture to the cell box by itself, but nothing here
        // zooms it back up, so the pixels are resized to the box first — the
        // same high-quality resize Sixel gets — and only the box travels:
        // fewer pixels for the compressor and fewer bytes on the wire, and a
        // payload whose own size is the box it is placed into.
        Backend::Kitty => {
            let pixel_width = (cols as u32 * cell.width.max(1) as u32).max(1);
            let pixel_height = (rows as u32 * cell.height.max(1) as u32).max(1);
            let scaled =
                decoded.resize_exact(pixel_width, pixel_height, FilterType::Triangle);
            let source = (scaled.width(), scaled.height());
            let png = encode_as_png(&scaled);
            let escape = kitty::place(&png, cols, rows, id, 0, rows, source);
            let info = PixelInfo::Kitty { source, id, png };
            (escape, Some(info))
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
            let source = (scaled.width(), scaled.height());
            let (header, bands) = sixel::encode(&scaled);
            let escape = sixel::whole(&header, &bands);
            (
                escape,
                Some(PixelInfo::Sixel {
                    source,
                    header,
                    bands,
                }),
            )
        }
        // `render` sends blocks to `fit`, which draws cells; only the two
        // pixel protocols ever reach here.
        Backend::Quad => unreachable!("quadrants are drawn by `fit`"),
    };

    Rendered {
        cols,
        rows,
        payload: Payload::Escape(escape),
        pixel,
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

    /// A PNG of the given size, so `render` can be driven the way the reader
    /// drives it: from bytes.
    fn encoded(width: u32, height: u32) -> Vec<u8> {
        crate::testkit::png(width, height, [0, 0, 0, 255])
    }

    fn encoded_as(width: u32, height: u32, format: image::ImageFormat) -> Vec<u8> {
        let mut bytes = std::io::Cursor::new(Vec::new());
        crate::testkit::image(width, height, [200, 100, 50, 255])
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
            Payload::Cells(_) => panic!("the kitty backend produced cells instead"),
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

        // And the payload is a PNG at the box it will be drawn in: nothing
        // else can travel under `f=100`, and the pixels past that box cost
        // only bytes and time for a zoom the reader cannot ask for.
        let png = encoded(20, 20);
        let carried = carried(&kitty_escape(&png));
        assert!(carried.starts_with(b"\x89PNG\r\n\x1a\n"), "not a PNG");
    }

    #[test]
    fn a_kitty_payload_is_encoded_at_the_box_it_will_be_shown_in() {
        // The payload travels once and the terminal shows it by id, so a
        // picture that is only ever drawn at its cell box has no use for the
        // pixels past it: they would be bytes on the wire and milliseconds on
        // the UI thread — a JPEG re-encoded at full resolution cost hundreds
        // of the latter per plate — for a zoom the reader cannot ask for.
        let jpeg = encoded_as(300, 400, image::ImageFormat::Jpeg);
        let cell = CellSize::default();
        let rendered = render(&jpeg, 40, 12, Role::Fill, Backend::Kitty, 1, cell).unwrap();
        let (cols, rows) = (rendered.width(), rendered.height());
        assert!(cols > 0 && rows > 0, "the fixture measured away");
        let payload = carried(rendered.escape().expect("kitty carries an escape"));
        assert_eq!(
            dimensions(&payload).unwrap(),
            (
                cols as u32 * cell.width as u32,
                rows as u32 * cell.height as u32
            ),
            "the payload is not the cell box"
        );
    }


    #[test]
    fn something_that_is_not_a_picture_is_an_error_not_a_panic() {
        for backend in [Backend::Quad, Backend::Kitty, Backend::Sixel] {
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
                let measured = measure(dimensions(&bytes).unwrap(), 40, 12, role, cell);
                for backend in [Backend::Quad, Backend::Kitty, Backend::Sixel] {
                    let rendered = render(&bytes, 40, 12, role, backend, 1, cell).unwrap();
                    assert_eq!(
                        (rendered.width() as u16, rendered.height() as u16),
                        measured,
                        "{role:?} {backend:?} with a {}x{} cell",
                        cell.width,
                        cell.height
                    );
                }
            }
        }
    }

    #[test]
    fn a_pixel_picture_says_what_a_write_is_built_from_and_a_block_one_has_nothing_to_say() {
        // The view places a picture only when it is pixels: the block backend
        // clips with the cells around it like any other text, while kitty and
        // sixel paint past the buffer and need the parts below.
        let bytes = encoded(64, 64);
        for backend in [Backend::Quad, Backend::Kitty, Backend::Sixel] {
            let rendered = render(
                &bytes,
                40,
                20,
                Role::Fill,
                backend,
                7,
                CellSize::default(),
            )
            .unwrap();
            assert_eq!(
                rendered.is_pixel(),
                backend != Backend::Quad,
                "{backend:?}"
            );
            match rendered.pixel() {
                None => assert!(rendered.escape().is_none(), "{backend:?}: no pixels, no escape"),
                Some(PixelInfo::Kitty { source, id, png }) => {
                    // The whole picture is one escape carrying the payload;
                    // the rectangle a cropped placement names its source from
                    // is the payload's own pixel size.
                    let escape = rendered.escape().unwrap();
                    assert!(
                        escape.contains(&format!("f=100,a=T,c={},r={}", rendered.width(), rendered.height())),
                        "{escape}"
                    );
                    assert_eq!(*source, (256, 256), "the payload is the cell box in pixels");
                    assert!(!png.is_empty(), "the payload travels with the placement");
                    assert_eq!(id, &7);
                }
                Some(PixelInfo::Sixel { source, header, bands }) => {
                    // The pieces the escape was built from, whole again.
                    assert_eq!(*source, (32 * 8, 16 * 16), "the cell box in pixels");
                    assert_eq!(
                        crate::image::sixel::whole(header, bands),
                        *rendered.escape().unwrap()
                    );
                }
            }
        }
    }
}

