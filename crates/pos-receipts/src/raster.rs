//! Receipt lines as bitmaps, for scripts a receipt printer has no characters for (Sinhala,
//! Tamil, Arabic, Devanagari, Chinese/Japanese, Korean: D-02, E20-T10).
//!
//! A thermal printer's code pages hold Latin, Greek and Cyrillic at best; anything else
//! used to be transliterated (`සීනි` → `siini`). A line that needs such a script is instead
//! shaped with the embedded Noto fonts (the PDF's, [`crate::pdf`]) and drawn as a 1-bit
//! image on the receipt's column grid, so it lines up with the printer's own text lines:
//! one column is [`CELL_DOTS`] dots, the width of an ESC/POS Font A character, and the
//! font size makes Noto Sans Mono exactly one cell wide. Other lines stay text: they print
//! faster and in the printer's own font.
//!
//! Glyph outlines are filled with `ab_glyph_rasterizer` and thresholded at
//! [`INK_THRESHOLD`] coverage. Not yet checked on a real printer (D-07).

// Pixel geometry is f32, as the shaper and the rasterizer use. No money passes through it.
#![allow(clippy::disallowed_types)]

use crate::pdf::{FontId, font, needs_script_font, runs};
use ab_glyph_rasterizer::{Rasterizer, point};

/// Dots per receipt column: an ESC/POS Font A character (12 × 24 dots at 203 dpi), so 32
/// columns are 384 dots (58 mm paper) and 48 are 576 (80 mm).
pub const CELL_DOTS: u32 = 12;

/// Coverage at which a dot is printed.
pub const INK_THRESHOLD: f32 = 0.45;

/// The font size in dots: Noto Sans Mono's advance is 0.6 em, so this makes it one cell.
const SIZE: f32 = CELL_DOTS as f32 / 0.6;

/// A 1-bit image, rows top to bottom, each row `width / 8` bytes, most significant bit
/// leftmost, 1 = ink (the ESC/POS raster format).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bitmap {
    /// In dots; a multiple of 8.
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Bitmap {
    fn new(width: u32, height: u32) -> Self {
        let width = width.div_ceil(8) * 8;
        Self {
            width,
            height,
            data: vec![0; (width / 8 * height) as usize],
        }
    }

    pub fn get(&self, x: u32, y: u32) -> bool {
        x < self.width
            && y < self.height
            && self.data[(y * self.width / 8 + x / 8) as usize] & (0x80 >> (x % 8)) != 0
    }

    fn set(&mut self, x: u32, y: u32) {
        if x < self.width && y < self.height {
            self.data[(y * self.width / 8 + x / 8) as usize] |= 0x80 >> (x % 8);
        }
    }

    /// Dots with ink.
    pub fn ink(&self) -> usize {
        self.data.iter().map(|b| b.count_ones() as usize).sum()
    }
}

/// Whether `text` has a character only a script font can draw (see the module notes).
pub fn line_needs_raster(text: &str) -> bool {
    text.chars().any(needs_script_font)
}

/// `text` (one receipt line, already laid out for `columns`) as a bitmap `columns` ×
/// [`CELL_DOTS`] wide. The height fits the tallest font the line uses, so marks above and
/// below Sinhala, Tamil and Devanagari letters are not cut.
pub fn render_line(text: &str, bold: bool, columns: usize) -> Bitmap {
    let base = if bold { FontId::MonoBold } else { FontId::Mono };
    // Each word starts at the column the text layout gave it, so a price after a name in
    // a script font still lands in its own columns (a script font draws spaces narrower or
    // wider than a cell).
    let words = words(text);
    let shaped: Vec<(usize, Vec<crate::pdf::Run>)> =
        words.iter().map(|(col, w)| (*col, runs(w, base))).collect();
    // Line box: the largest ascent and descent of the fonts in the line.
    let (mut ascent, mut descent) = metrics(base);
    for r in shaped.iter().flat_map(|(_, rs)| rs) {
        let (a, d) = metrics(r.font);
        ascent = ascent.max(a);
        descent = descent.max(d);
    }
    let baseline = (ascent * SIZE).ceil();
    let height = (baseline + (descent * SIZE).ceil()) as u32;
    let mut bitmap = Bitmap::new(columns as u32 * CELL_DOTS, height.max(1));
    let cell = CELL_DOTS as f32;
    for (start, runs) in &shaped {
        let mut col = *start;
        for r in runs {
            let x0 = col as f32 * cell;
            let room = r.columns as f32 * cell;
            let width = r.advance * SIZE;
            // As in the PDF: a run wider than its columns is narrowed to fit.
            let squeeze = if width > room && room > 0.0 {
                room / width
            } else {
                1.0
            };
            let face = &font(r.font).shaper;
            let scale = SIZE / face.units_per_em() as f32;
            let mut pen = 0.0;
            for g in &r.placed {
                let ox = x0 + (pen + g.x_offset) * SIZE * squeeze;
                let oy = baseline - g.y_offset * SIZE;
                draw_glyph(&mut bitmap, face, g.id, ox, oy, scale, squeeze);
                pen += g.x_advance;
            }
            col += r.columns;
        }
    }
    bitmap
}

/// The words of `text` (split at spaces) with the column each starts in.
fn words(text: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut col = 0;
    let mut in_word = false;
    for c in text.chars() {
        if c == ' ' {
            in_word = false;
        } else {
            match out.last_mut() {
                Some((_, w)) if in_word => w.push(c),
                _ => out.push((col, c.to_string())),
            }
            in_word = true;
        }
        col += crate::text::char_columns(c);
    }
    out
}

/// Ascent and descent of a font, in ems (positive numbers).
fn metrics(id: FontId) -> (f32, f32) {
    let face = &font(id).shaper;
    let upem = face.units_per_em() as f32;
    (
        f32::from(face.ascender()) / upem,
        -f32::from(face.descender()) / upem,
    )
}

/// Fills one glyph's outline into `bitmap` with its origin at (`ox`, `oy`) in dots.
fn draw_glyph(
    bitmap: &mut Bitmap,
    face: &rustybuzz::Face<'static>,
    id: u16,
    ox: f32,
    oy: f32,
    scale: f32,
    squeeze: f32,
) {
    let glyph = rustybuzz::ttf_parser::GlyphId(id);
    let Some(bbox) = face.glyph_bounding_box(glyph) else {
        return; // a space, or an empty glyph
    };
    let sx = scale * squeeze;
    // The glyph's box in dots, with a dot of margin for anti-aliasing.
    let left = (ox + f32::from(bbox.x_min) * sx).floor() - 1.0;
    let top = (oy - f32::from(bbox.y_max) * scale).floor() - 1.0;
    let right = (ox + f32::from(bbox.x_max) * sx).ceil() + 1.0;
    let bottom = (oy - f32::from(bbox.y_min) * scale).ceil() + 1.0;
    let (w, h) = ((right - left).max(1.0) as usize, (bottom - top).max(1.0) as usize);
    let mut outline = Outline {
        r: Rasterizer::new(w, h),
        map: move |x: f32, y: f32| point(ox + x * sx - left, oy - y * scale - top),
        start: point(0.0, 0.0),
        last: point(0.0, 0.0),
    };
    if face.outline_glyph(glyph, &mut outline).is_none() {
        return;
    }
    outline.r.for_each_pixel_2d(|x, y, coverage| {
        if coverage >= INK_THRESHOLD {
            let (px, py) = (left + x as f32, top + y as f32);
            if px >= 0.0 && py >= 0.0 {
                bitmap.set(px as u32, py as u32);
            }
        }
    });
}

/// Feeds a glyph outline (font units, y up) to the rasterizer (dots, y down).
struct Outline<F: Fn(f32, f32) -> ab_glyph_rasterizer::Point> {
    r: Rasterizer,
    map: F,
    start: ab_glyph_rasterizer::Point,
    last: ab_glyph_rasterizer::Point,
}

impl<F: Fn(f32, f32) -> ab_glyph_rasterizer::Point> rustybuzz::ttf_parser::OutlineBuilder for Outline<F> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.start = (self.map)(x, y);
        self.last = self.start;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = (self.map)(x, y);
        self.r.draw_line(self.last, p);
        self.last = p;
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (c, p) = ((self.map)(x1, y1), (self.map)(x, y));
        self.r.draw_quad(self.last, c, p);
        self.last = p;
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (c1, c2, p) = ((self.map)(x1, y1), (self.map)(x2, y2), (self.map)(x, y));
        self.r.draw_cubic(self.last, c1, c2, p);
        self.last = p;
    }
    fn close(&mut self) {
        if self.last != self.start {
            self.r.draw_line(self.last, self.start);
        }
        self.last = self.start;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_lines_with_a_script_font_are_images() {
        for t in ["Tea  2.50", "Café Crème", "Ωμέγα", "Хлеб", "R01-000001", "£ € ¥"] {
            assert!(!line_needs_raster(t), "{t}");
        }
        for t in ["සීනි 1kg", "பால்", "كولا", "मसाला", "可口可乐", "콜라"]
        {
            assert!(line_needs_raster(t), "{t}");
        }
    }

    #[test]
    fn a_line_is_as_wide_as_its_columns_and_tall_enough_for_its_marks() {
        let latin = render_line("Tea", false, 48);
        assert_eq!(latin.width, 576);
        let sinhala = render_line("සීනි", false, 32);
        assert_eq!(sinhala.width, 384);
        assert!(
            sinhala.height >= latin.height,
            "{} < {}",
            sinhala.height,
            latin.height
        );
        assert!(sinhala.ink() > 0);
        assert_eq!(render_line("    ", false, 48).ink(), 0);
    }

    /// Ink stays in the columns its text was laid out in: a name in columns 0–9 leaves the
    /// amount's columns (40–47) empty, so prices stay aligned with the printer's text.
    #[test]
    fn ink_stays_in_its_columns() {
        let b = render_line("සීනි පාන්", false, 48);
        for y in 0..b.height {
            for x in 10 * CELL_DOTS..b.width {
                assert!(!b.get(x, y), "ink at ({x}, {y})");
            }
        }
        let name = "சர்க்கரை";
        let pad = " ".repeat(42 - crate::text::text_columns(name));
        let with_amount = render_line(&format!("{name}{pad}{:>6}", "250.00"), false, 48);
        let amount_ink = (0..with_amount.height)
            .flat_map(|y| (42 * CELL_DOTS..48 * CELL_DOTS).map(move |x| (x, y)))
            .filter(|(x, y)| with_amount.get(*x, *y))
            .count();
        assert!(amount_ink > 0, "the amount is drawn in its columns");
    }
}
