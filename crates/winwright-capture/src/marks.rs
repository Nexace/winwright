//! Numbered boxes drawn on a screenshot ("set of marks"): the model names an element by its
//! number instead of guessing pixels. Pure pixel code; a tiny built-in font, digits only.

use winwright_contracts::capture::Mark;
use winwright_contracts::geometry::PhysicalPoint;

use crate::raster::{BYTES_PER_PIXEL, Bgra};

/// 3x5 digit glyphs, one row per entry, high bit on the left.
const DIGITS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b010, 0b110, 0b010, 0b010, 0b111],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b001, 0b111],
    [0b101, 0b101, 0b111, 0b001, 0b001],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b101, 0b111],
    [0b111, 0b001, 0b001, 0b001, 0b001],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b001, 0b111],
];

/// Box colors as `0xRRGGBB`, taken in turn so neighbours differ. White digits read on each.
const COLORS: [u32; 6] = [
    0xD9_2D_2D, 0x0E_74_90, 0x7C_3A_ED, 0xC2_41_0C, 0x15_80_3D, 0xBE_18_5D,
];
const WHITE: u32 = 0xFF_FF_FF;

/// Glyph pixel size so the digits stay legible once the image is scaled by `scale` (<= 1).
pub fn glyph_size(scale: f64) -> i32 {
    ((2.0 / scale.clamp(0.05, 1.0)).ceil() as i32).max(2)
}

fn put(img: &mut Bgra, x: i32, y: i32, rgb: u32) {
    if x < 0 || y < 0 || x >= img.width as i32 || y >= img.height as i32 {
        return;
    }
    let i = (y as usize * img.width as usize + x as usize) * BYTES_PER_PIXEL;
    img.pixels[i] = (rgb & 0xFF) as u8;
    img.pixels[i + 1] = (rgb >> 8 & 0xFF) as u8;
    img.pixels[i + 2] = (rgb >> 16 & 0xFF) as u8;
    img.pixels[i + 3] = 255;
}

fn fill(img: &mut Bgra, (x, y, w, h): (i32, i32, i32, i32), rgb: u32) {
    for yy in y.max(0)..(y + h).min(img.height as i32) {
        for xx in x.max(0)..(x + w).min(img.width as i32) {
            put(img, xx, yy, rgb);
        }
    }
}

/// Draws each mark (desktop pixels; the image's top-left is `origin`): an outline in its color
/// and its number in a tag at the top-left corner, digits `k` image pixels per font pixel.
pub fn draw(img: &mut Bgra, origin: PhysicalPoint, marks: &[Mark], k: i32) {
    let line = (k / 2).max(1);
    for (i, mark) in marks.iter().enumerate() {
        let color = COLORS[i % COLORS.len()];
        let (x, y) = (mark.rect.left - origin.x, mark.rect.top - origin.y);
        let (w, h) = (mark.rect.width(), mark.rect.height());
        fill(img, (x, y, w, line), color);
        fill(img, (x, y + h - line, w, line), color);
        fill(img, (x, y, line, h), color);
        fill(img, (x + w - line, y, line, h), color);
        let text = mark.number.to_string();
        let tag_w = text.len() as i32 * 4 * k + k;
        let tag_h = 7 * k;
        fill(img, (x, y, tag_w, tag_h), color);
        for (n, digit) in text.bytes().enumerate() {
            let glyph = DIGITS[usize::from(digit - b'0')];
            let gx = x + k + n as i32 * 4 * k;
            for (row, bits) in glyph.iter().enumerate() {
                for col in 0..3 {
                    if bits >> (2 - col) & 1 == 1 {
                        let px = (gx + col * k, y + k + row as i32 * k, k, k);
                        fill(img, px, WHITE);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winwright_contracts::geometry::PhysicalRect;

    fn at(img: &Bgra, x: u32, y: u32) -> [u8; 4] {
        let i = (y * img.width + x) as usize * 4;
        img.pixels[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn a_mark_outlines_its_rect_and_tags_its_number() {
        let mut img = Bgra::black(60, 40).unwrap();
        let origin = PhysicalPoint { x: 100, y: 200 };
        let mark = Mark {
            rect: PhysicalRect::new(110, 205, 150, 235),
            number: 7,
        };
        draw(&mut img, origin, &[mark], 2);
        let red = [0x2D, 0x2D, 0xD9, 255];
        assert_eq!(at(&img, 49, 20), red, "right edge outlined");
        assert_eq!(at(&img, 30, 34), red, "bottom edge outlined");
        assert_eq!(at(&img, 30, 20), [0, 0, 0, 255], "inside stays clear");
        // "7": its top row is white, the tag around it red.
        assert_eq!(at(&img, 12, 7), [255, 255, 255, 255]);
        assert_eq!(at(&img, 11, 6), red);
    }

    #[test]
    fn marks_off_the_image_are_clipped_not_panicking() {
        let mut img = Bgra::black(10, 10).unwrap();
        let mark = Mark {
            rect: PhysicalRect::new(-50, -50, 500, 500),
            number: 123,
        };
        draw(&mut img, PhysicalPoint { x: 0, y: 0 }, &[mark], 3);
    }

    #[test]
    fn digits_grow_as_the_image_shrinks() {
        assert_eq!(glyph_size(1.0), 2);
        assert_eq!(glyph_size(0.706), 3);
        assert_eq!(glyph_size(0.4), 5);
    }
}
