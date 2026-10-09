//! The block backend: one cell, four quadrants, two colours.
//!
//! This is the path that works in every terminal, through tmux too: the
//! picture is sampled one pixel per quadrant and drawn as an ordinary glyph,
//! so nothing here speaks a graphics protocol.

use super::{Cell, CellSize, Payload, Rendered, Role, flatten, measure};
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView};

/// Renders a picture as block glyphs.
pub(super) fn fit(
    decoded: &DynamicImage,
    max_cols: u16,
    max_rows: u16,
    role: Role,
    cell: CellSize,
) -> Rendered {
    let (cols, rows) = measure(decoded.dimensions(), max_cols, max_rows, role, cell);
    if cols == 0 || rows == 0 {
        return Rendered {
            cols: 0,
            rows: 0,
            payload: Payload::Cells(Vec::new()),
            // Blocks are cells: nothing paints past the buffer, so there is
            // no crop to carry.
            pixel: None,
        };
    }

    // The picture is sampled one pixel per quadrant: two across a cell, two
    // down. The cell box and the picture's shape were settled by `measure`,
    // so this only reads the pixels.
    let scaled = decoded
        .resize_exact(cols as u32 * 2, rows as u32 * 2, FilterType::Triangle)
        .to_rgba8();
    let mut out = Vec::with_capacity(rows as usize);
    for row in 0..rows as u32 {
        let mut cells = Vec::with_capacity(cols as usize);
        for column in 0..cols as u32 {
            cells.push(quadrant_of(&scaled, column * 2, row * 2));
        }
        out.push(cells);
    }
    Rendered {
        cols,
        rows,
        payload: Payload::Cells(out),
        // A block picture is ordinary text as far as the screen is concerned:
        // it clips with the cells around it and never asks for a placement.
        pixel: None,
    }
}

/// The glyph of each of the sixteen ways four quadrants can be lit.
///
/// Bit 0 is the top-left quadrant, bit 1 the top-right, bit 2 the bottom-left
/// and bit 3 the bottom-right — the order the four pixels of a cell arrive in.
const QUAD: [char; 16] = [
    ' ', '▘', '▝', '▀', '▖', '▌', '▞', '▛', '▗', '▚', '▐', '▜', '▄', '▙', '▟', '█',
];

/// The four quadrants of one cell: the cell's own 2x2 block of pixels, lit
/// where the pixel is brighter than their mean.
fn quadrant_of(image: &image::RgbaImage, x0: u32, y0: u32) -> Cell {
    let mut pixels = [(0u8, 0u8, 0u8); 4];
    let mut bright = [0u32; 4];
    for dy in 0..2u32 {
        for dx in 0..2u32 {
            let [r, g, b, a] = image.get_pixel(x0 + dx, y0 + dy).0;
            let rgb = flatten([r, g, b, a]);
            let at = (dy * 2 + dx) as usize;
            pixels[at] = rgb;
            bright[at] = luminance(rgb);
        }
    }
    // The mean is compared without dividing: two pixels of one colour have to
    // compare equal, and a float mean would sit a hair above the value it is
    // the mean of.
    let sum: u32 = bright.iter().sum();

    let mut glyph = 0usize;
    let mut lit = Vec::with_capacity(4);
    let mut dark = Vec::with_capacity(4);
    for (at, pixel) in pixels.iter().enumerate() {
        if u64::from(bright[at]) * 4 >= u64::from(sum) {
            glyph |= 1 << at;
            lit.push(*pixel);
        } else {
            dark.push(*pixel);
        }
    }
    let fallback = mean_rgb(&pixels);
    Cell {
        ch: QUAD[glyph],
        fg: if lit.is_empty() {
            fallback
        } else {
            mean_rgb(&lit)
        },
        bg: if dark.is_empty() {
            fallback
        } else {
            mean_rgb(&dark)
        },
    }
}

/// How bright a pixel reads, for telling a cell's bright pixels from its dark
/// ones.
///
/// The channels are weighted the way a display weighs them, in whole numbers:
/// two pixels of one colour must compare equal, and a fractional mean would
/// not let them.
fn luminance((r, g, b): (u8, u8, u8)) -> u32 {
    2126 * r as u32 + 7152 * g as u32 + 722 * b as u32
}

fn mean_rgb(pixels: &[(u8, u8, u8)]) -> (u8, u8, u8) {
    let count = pixels.len().max(1) as u32;
    let mut sums = [0u32; 3];
    for (r, g, b) in pixels {
        sums[0] += *r as u32;
        sums[1] += *g as u32;
        sums[2] += *b as u32;
    }
    (
        (sums[0] / count) as u8,
        (sums[1] / count) as u8,
        (sums[2] / count) as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::render;
    use image::{DynamicImage, Rgba, RgbaImage};

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
            &crate::testkit::image(100, 100, [10, 20, 30, 255]),
            40,
            100,
            Role::Fill,
            cell,
        );
        assert!(rendered.width() <= 40);
        assert!(rendered.height() >= 1);
        // A square picture takes the cells its shape needs: a cell one wide
        // and two tall makes it half as many rows as columns.
        let square = fit(
            &crate::testkit::image(100, 100, [0, 0, 0, 255]),
            40,
            100,
            Role::Fill,
            cell,
        );
        let expected = square.width() as f32 * cell.width as f32 / cell.height as f32;
        assert!(
            (square.height() as f32 - expected).abs() <= 1.0,
            "{} rows for {} columns",
            square.height(),
            square.width()
        );
        let tall = fit(
            &crate::testkit::image(100, 1000, [0, 0, 0, 255]),
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
    fn a_flat_cell_is_one_block_of_its_own_colour() {
        // A cell whose pixels cannot be told apart fills with one colour, so a
        // photograph's flat areas stay flat instead of turning into noise.
        let rendered = fit(
            &crate::testkit::image(10, 10, [200, 100, 50, 255]),
            10,
            10,
            Role::Fill,
            CellSize::default(),
        );
        let cell = rendered.cells().unwrap()[0][0];
        assert_eq!(cell.ch, '█', "a filled block");
        assert_eq!(cell.fg, (200, 100, 50));
        assert_eq!(cell.bg, (200, 100, 50));

        // Transparency is resolved against white, the colour the author of a
        // line drawing assumed, not against the terminal's black.
        assert_eq!(flatten([0, 0, 0, 255]), (0, 0, 0));
        assert_eq!(flatten([0, 0, 0, 0]), (255, 255, 255));
        let half = flatten([0, 0, 0, 128]);
        assert!(half.0 > 100 && half.0 < 160, "{half:?}");
    }

    #[test]
    fn the_quadrants_are_the_bright_pixels_of_their_cell() {
        // A 2x2 picture is one cell, quadrant for quadrant: the one dark pixel
        // of the block is the one quadrant that is not lit — the top-left,
        // whose absence leaves `▟` — and the two colours are the means of the
        // lit and unlit pixels.
        let mut buffer = RgbaImage::new(2, 2);
        for pixel in buffer.pixels_mut() {
            *pixel = Rgba([255, 255, 255, 255]);
        }
        buffer.put_pixel(0, 0, Rgba([0, 0, 0, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(buffer)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();

        let rendered = render(
            &bytes.into_inner(),
            10,
            10,
            Role::Keep,
            crate::image::Backend::Quad,
            1,
            CellSize {
                width: 2,
                height: 2,
            },
        )
        .unwrap();
        assert_eq!((rendered.width(), rendered.height()), (1, 1));
        let cell = rendered.cells().unwrap()[0][0];
        assert_eq!(cell.ch, '▟', "every quadrant but the top-left is lit");
        assert_eq!(cell.fg, (255, 255, 255), "the lit quadrants are white");
        assert_eq!(cell.bg, (0, 0, 0), "the one dark quadrant is black");
    }
}
