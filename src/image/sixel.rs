//! Encoding pictures as Sixel, the format foot and xterm understand.
//!
//! Sixel writes an image in bands six pixel rows tall. Within a band, one
//! character carries six vertically stacked pixels of one colour: bit 0 is the
//! top row, bit 5 the bottom, and the value is offset by 63 into printable
//! ASCII. A band is written once per colour that appears in it, returning to the
//! band's start with `$` in between, and `-` moves on to the next band.
//!
//! Colours come from a fixed palette rather than a quantiser. A 6·6·6 colour
//! cube plus a grey ramp covers book illustrations well, keeps the encoder
//! simple, and costs no dependency. Sixel palettes hold 256 entries, so the cube
//! has to stay within that budget.

use image::RgbaImage;

/// Levels per channel in the colour cube.
const LEVELS: usize = 6;
/// Extra grey steps, which line drawings and screenshots rely on.
const GREYS: usize = 24;

/// Builds the palette: a colour cube followed by a grey ramp.
fn palette() -> Vec<(u8, u8, u8)> {
    let mut colors = Vec::with_capacity(LEVELS * LEVELS * LEVELS + GREYS);
    for r in 0..LEVELS {
        for g in 0..LEVELS {
            for b in 0..LEVELS {
                colors.push((step(r), step(g), step(b)));
            }
        }
    }
    for grey in 0..GREYS {
        let value = (grey * 255 / (GREYS - 1)) as u8;
        colors.push((value, value, value));
    }
    colors
}

fn step(index: usize) -> u8 {
    (index * 255 / (LEVELS - 1)) as u8
}

/// Squared distance between two colours.
fn distance((ar, ag, ab): (u8, u8, u8), (br, bg, bb): (u8, u8, u8)) -> u32 {
    (ar as i32 - br as i32).pow(2) as u32
        + (ag as i32 - bg as i32).pow(2) as u32
        + (ab as i32 - bb as i32).pow(2) as u32
}

/// The cube level closest to one channel value.
///
/// The levels sit at 0, 51, 102, 153, 204 and 255, so the nearest one is the
/// value divided by the spacing and rounded. No value lands exactly between two
/// levels, because the spacing is odd.
fn level_of(value: u8) -> usize {
    ((value as u32 * (LEVELS as u32 - 1) + 127) / 255) as usize
}

/// Nearest palette entry for a pixel, by squared distance.
///
/// The cube is a regular grid, so its nearest point follows from rounding each
/// channel on its own: on a rectangular grid that minimises the euclidean
/// distance, and no search is needed. Only the grey ramp is scanned, and it
/// holds a tenth of the palette. Searching all entries cost a book of rendered
/// formulas about sixteen seconds a chapter.
///
/// Ties go to the lower index, as a scan over the whole palette would give.
fn nearest(palette: &[(u8, u8, u8)], pixel: (u8, u8, u8)) -> usize {
    let (r, g, b) = pixel;
    let cube = level_of(r) * LEVELS * LEVELS + level_of(g) * LEVELS + level_of(b);
    let mut best = cube;
    let mut best_distance = distance(palette[cube], pixel);

    let greys = LEVELS * LEVELS * LEVELS;
    for (index, colour) in palette.iter().enumerate().skip(greys) {
        let candidate = distance(*colour, pixel);
        if candidate < best_distance {
            best_distance = candidate;
            best = index;
        }
    }
    best
}

/// Encodes an image as Sixel, in the pieces a crop writes: the palette header,
/// then one string per band — each band carrying the `-` that moves on to the
/// next one while another follows, and none of them the terminator, which the
/// writer puts on the end.
///
/// The image must already be scaled to its final pixel size, because Sixel
/// carries no scaling of its own.
pub fn encode(image: &RgbaImage) -> (String, Vec<String>) {
    let (width, height) = (image.width(), image.height());
    if width == 0 || height == 0 {
        return (String::new(), Vec::new());
    }
    let palette = palette();

    // Map every pixel to a palette index once, so the per-colour passes below
    // only compare integers.
    let mut indexed = vec![0u16; (width * height) as usize];
    let mut used = vec![false; palette.len()];
    for (x, y, pixel) in image.enumerate_pixels() {
        let index = nearest(&palette, super::flatten(pixel.0));
        indexed[(y * width + x) as usize] = index as u16;
        used[index] = true;
    }

    // P q introduces the data; the raster attributes give the aspect ratio 1:1
    // and the pixel size, which lets a terminal reserve the right area.
    let mut header = String::with_capacity(64);
    header.push_str("\x1bP0;1;0q\"1;1;");
    header.push_str(&format!("{width};{height}"));

    for (index, &(r, g, b)) in palette.iter().enumerate() {
        if !used[index] {
            continue;
        }
        // Sixel colour components are percentages.
        header.push_str(&format!(
            "#{};2;{};{};{}",
            index,
            percent(r),
            percent(g),
            percent(b)
        ));
    }

    let count = height.div_ceil(6);
    let mut bands = Vec::with_capacity(count as usize);
    for band in 0..count {
        let mut text = String::with_capacity((width / 4) as usize + 64);
        let mut first_colour = true;
        for (colour, is_used) in used.iter().enumerate() {
            if !is_used {
                continue;
            }
            // Collect this colour's pixels across the band.
            let mut run: Vec<u8> = Vec::new();
            let mut any = false;
            for x in 0..width {
                let mut bits = 0u8;
                for row in 0..6u32 {
                    let y = band * 6 + row;
                    if y >= height {
                        break;
                    }
                    if indexed[(y * width + x) as usize] as usize == colour {
                        bits |= 1 << row;
                    }
                }
                if bits != 0 {
                    any = true;
                }
                run.push(bits);
            }
            if !any {
                continue;
            }
            if !first_colour {
                // Return to the start of the band for the next colour.
                text.push('$');
            }
            first_colour = false;
            text.push_str(&format!("#{colour}"));
            write_run(&mut text, &run);
        }
        if band + 1 < count {
            text.push('-');
        }
        bands.push(text);
    }

    (header, bands)
}

/// The whole escape: header, every band in order, terminator — what a frame
/// that shows a picture in full writes, and what the pieces of `encode` must
/// add up to byte for byte.
pub fn whole(header: &str, bands: &[String]) -> String {
    if header.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(header.len() + 2);
    out.push_str(header);
    for band in bands {
        out.push_str(band);
    }
    out.push_str("\x1b\\");
    out
}

/// Writes one colour's band, compressing repeats with the `!` run operator.
fn write_run(out: &mut String, run: &[u8]) {
    let mut index = 0;
    while index < run.len() {
        let bits = run[index];
        let mut length = 1;
        while index + length < run.len() && run[index + length] == bits {
            length += 1;
        }
        let glyph = (bits + 63) as char;
        // The operator only pays off from four repeats.
        if length >= 4 {
            out.push_str(&format!("!{length}{glyph}"));
        } else {
            for _ in 0..length {
                out.push(glyph);
            }
        }
        index += length;
    }
}

fn percent(value: u8) -> u8 {
    ((value as u16 * 100) / 255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    /// The picture `encode` reads, built the shared way: one flat colour at
    /// the size the test names.
    fn solid(width: u32, height: u32, color: [u8; 4]) -> RgbaImage {
        crate::testkit::image(width, height, color).to_rgba8()
    }

    /// The whole escape for a picture, which is what a frame that shows it
    /// writes.
    fn encoded(image: &RgbaImage) -> String {
        let (header, bands) = encode(image);
        whole(&header, &bands)
    }

    /// Six columns of two alternating colours over twelve rows: two bands,
    /// each of them carrying both colours, so a band range has something in
    /// it to tell apart.
    fn checkerboard() -> RgbaImage {
        let mut image = RgbaImage::new(6, 12);
        for (x, y, pixel) in image.enumerate_pixels_mut() {
            *pixel = Rgba(if (x + y) % 2 == 0 {
                [200, 40, 60, 255]
            } else {
                [30, 160, 90, 255]
            });
        }
        image
    }

    #[test]
    fn an_encoded_image_is_well_formed() {
        let escape = encoded(&solid(4, 6, [255, 0, 0, 255]));
        assert!(escape.starts_with("\x1bP"), "missing introducer");
        assert!(escape.contains("q\"1;1;4;6"), "missing raster attributes");
        assert!(escape.ends_with("\x1b\\"), "missing terminator");
        assert!(escape.contains("#"), "no colour defined");
        // A palette past 256 entries is not a palette sixel can hold.
        assert!(palette().len() <= 256, "{} entries", palette().len());
    }

    #[test]
    fn bands_runs_and_a_second_colour_are_written_out() {
        // Six rows of one colour means all six bits, so 63 + 63 = '~'.
        let escape = encoded(&solid(1, 6, [0, 0, 0, 255]));
        assert!(escape.contains('~'), "{escape:?}");
        // Twelve rows are two bands.
        let escape = encoded(&solid(2, 12, [0, 0, 0, 255]));
        assert_eq!(escape.matches('-').count(), 1, "expected one band break");
        // A run of twenty is worth compressing; three identical columns are
        // below the threshold for `!` and stay literal.
        let escape = encoded(&solid(20, 6, [0, 0, 0, 255]));
        assert!(escape.contains("!20"), "expected a run of 20: {escape:?}");
        let escape = encoded(&solid(3, 6, [0, 0, 0, 255]));
        assert!(!escape.contains('!'), "{escape:?}");
        // A second colour in the same band needs the carriage return.
        let mut buffer = RgbaImage::new(2, 6);
        for y in 0..6 {
            buffer.put_pixel(0, y, Rgba([255, 0, 0, 255]));
            buffer.put_pixel(1, y, Rgba([0, 0, 255, 255]));
        }
        let escape = encoded(&buffer);
        assert!(escape.contains('$'), "{escape:?}");
    }

    #[test]
    fn the_pieces_add_up_to_the_escape_the_encoder_always_wrote() {
        // Pinned from the encoder before it was split into pieces: a frame
        // that shows a whole picture must write byte for byte what it wrote
        // then, header, bands and terminator alike.
        let old = "\x1bP0;1;0q\"1;1;6;12#56;2;20;60;40#151;2;80;20;20#56iTiTiT$#151TiTiTi-#56iTiTiT$#151TiTiTi\x1b\\";
        let (header, bands) = encode(&checkerboard());
        assert_eq!(bands.len(), 2, "twelve rows are two bands");
        assert_eq!(whole(&header, &bands), old);
        assert_eq!(format!("{header}{}\x1b\\", bands.concat()), old);
    }

    #[test]
    fn a_crop_writes_the_bands_it_covers_and_the_terminator() {
        // A picture is cropped to whole bands, never inside one: the second
        // band of the pinned escape above, from just past the first band's
        // `-` to the end, is the whole of what such a crop says.
        let (header, bands) = encode(&checkerboard());
        assert_eq!(
            whole(&header, &bands[1..2]),
            format!("{header}#56iTiTiT$#151TiTiTi\x1b\\")
        );
        // And every band in range is the whole picture again.
        assert_eq!(whole(&header, &bands[0..]), whole(&header, &bands));
    }
}
