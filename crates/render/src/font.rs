//! Faces, cell metrics and rasterization.
//!
//! The terminal face is embedded, not looked up: the cell size then depends on
//! nothing but the size setting, so every machine lays out the same grid, and
//! there is no fontconfig scan on cold start (§4, 250 ms).
//!
//! Glyphs are rasterized with TrueType hinting at integer pixel positions. The
//! cell width is rounded to whole pixels, so every column starts on a pixel
//! boundary and one bitmap per glyph serves every column (no subpixel buckets,
//! §5.7's cache key collapses to the glyph). That is what Alacritty and kitty
//! do; the canvas path used fractional advances and drew glyph by glyph to hide
//! the drift (§18.8).

use swash::scale::{Render, ScaleContext, Source};
use swash::zeno::Format;
use swash::FontRef;

static REGULAR: &[u8] = include_bytes!("../fonts/JetBrainsMonoNL-Regular.ttf");
static BOLD: &[u8] = include_bytes!("../fonts/JetBrainsMonoNL-Bold.ttf");
static ITALIC: &[u8] = include_bytes!("../fonts/JetBrainsMonoNL-Italic.ttf");
static BOLD_ITALIC: &[u8] = include_bytes!("../fonts/JetBrainsMonoNL-BoldItalic.ttf");

/// Which of the four faces. The discriminant indexes [`Fonts::faces`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Face {
    Regular = 0,
    Bold = 1,
    Italic = 2,
    BoldItalic = 3,
}

impl Face {
    pub fn new(bold: bool, italic: bool) -> Face {
        match (bold, italic) {
            (false, false) => Face::Regular,
            (true, false) => Face::Bold,
            (false, true) => Face::Italic,
            (true, true) => Face::BoldItalic,
        }
    }
}

/// Cell geometry in physical pixels. Everything is an integer: a fractional
/// cell would put column boundaries between pixels and blur every glyph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellMetrics {
    pub width: u32,
    pub height: u32,
    /// Baseline, from the top of the cell.
    pub baseline: u32,
    /// Underline top, from the top of the cell.
    pub underline: u32,
    pub stroke: u32,
    /// Strikethrough top, from the top of the cell.
    pub strike: u32,
}

/// A rasterized glyph: an 8-bit coverage mask and where it sits relative to the
/// cell's top-left corner.
#[derive(Clone, Debug, Default)]
pub struct Bitmap {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Bitmap {
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

pub struct Fonts {
    faces: [FontRef<'static>; 4],
    ctx: ScaleContext,
    px: f32,
    pub metrics: CellMetrics,
}

impl Fonts {
    /// `px` is the font size in physical pixels (CSS px × scale factor), and
    /// `line_height` the cell height in physical pixels, which the UI already
    /// owns as a setting (`lineHeightPx`).
    pub fn new(px: f32, line_height: u32) -> Fonts {
        let face = |d: &'static [u8]| FontRef::from_index(d, 0).expect("embedded font parses");
        let faces = [face(REGULAR), face(BOLD), face(ITALIC), face(BOLD_ITALIC)];
        let px = px.max(4.0);
        let regular = faces[0];
        let m = regular.metrics(&[]).scale(px);
        let gid = regular.charmap().map('M');
        let advance = regular.glyph_metrics(&[]).scale(px).advance_width(gid);

        let width = advance.round().max(1.0) as u32;
        let height = line_height.max((m.ascent + m.descent).ceil() as u32).max(1);
        // Centre the font's line box in the cell, as the canvas did with
        // `textBaseline = "middle"`: the extra line height splits above and below.
        let pad = (height as f32 - (m.ascent + m.descent)) / 2.0;
        let baseline = (pad + m.ascent).round() as u32;
        let stroke = m.stroke_size.round().max(1.0) as u32;
        // underline_offset is negative below the baseline in swash's metrics.
        let underline = (baseline as f32 - m.underline_offset).round() as u32;
        let underline = underline.min(height - stroke);
        let strike = (baseline as f32 - m.strikeout_offset.max(m.x_height / 2.0)).round().max(0.0) as u32;

        Fonts {
            faces,
            ctx: ScaleContext::new(),
            px,
            metrics: CellMetrics { width, height, baseline, underline, stroke, strike },
        }
    }

    pub fn px(&self) -> f32 {
        self.px
    }

    /// Does the embedded face have this character?
    pub fn covers(&self, ch: char) -> bool {
        self.faces[0].charmap().map(ch) != 0
    }

    /// Rasterize one character. `cells` is 2 for a wide character.
    ///
    /// Box drawing and block elements are drawn, not rasterized: the font's
    /// versions are sized to the em box, and with a line height above 1.0 the
    /// vertical strokes stop short of the cell edge and `tree` output turns
    /// into dashed lines.
    pub fn rasterize(&mut self, ch: char, face: Face, cells: u32) -> Bitmap {
        if let Some(b) = crate::boxdraw::draw(ch, self.metrics, face) {
            return b;
        }
        let font = self.faces[face as usize];
        let mut gid = font.charmap().map(ch);
        if gid == 0 {
            // Not in the face. A system fallback chain is future work; until
            // then, the replacement character, which the subset keeps.
            gid = font.charmap().map('\u{FFFD}');
        }
        let mut scaler = self.ctx.builder(font).size(self.px).hint(true).build();
        let Some(img) = Render::new(&[Source::Outline]).format(Format::Alpha).render(&mut scaler, gid) else {
            return Bitmap::default();
        };
        let p = img.placement;
        let mut b = Bitmap {
            // Placement is relative to the pen on the baseline; y grows up.
            left: p.left,
            top: self.metrics.baseline as i32 - p.top,
            width: p.width,
            height: p.height,
            data: img.data,
        };
        // A wide glyph from a narrow face is centred across its two cells.
        if cells == 2 {
            let adv = font.glyph_metrics(&[]).scale(self.px).advance_width(gid);
            b.left += ((2 * self.metrics.width) as f32 - adv).max(0.0) as i32 / 2;
        }
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_at_the_default_size_are_whole_pixels_and_sane() {
        // The app's defaults: 12.5 px, line height round(12.5 * 1.36) = 17.
        let f = Fonts::new(12.5, 17);
        let m = f.metrics;
        assert_eq!(m.width, 8, "JetBrains Mono advance is 0.6 em: 7.5 rounds to 8");
        assert_eq!(m.height, 17);
        assert!(m.baseline > m.height / 2 && m.baseline < m.height, "{m:?}");
        assert!(m.underline > m.baseline && m.underline + m.stroke <= m.height, "{m:?}");
        assert!(m.strike < m.baseline, "{m:?}");
    }

    #[test]
    fn printable_ascii_rasterizes_inside_its_cell() {
        let mut f = Fonts::new(12.5, 17);
        let m = f.metrics;
        for c in '!'..='~' {
            let b = f.rasterize(c, Face::Regular, 1);
            assert!(!b.is_empty(), "{c:?} rendered nothing");
            assert_eq!(b.data.len(), (b.width * b.height) as usize);
            // A little overhang is normal (italic, `_`); a glyph a whole cell
            // away is a placement bug.
            assert!(b.left >= -2 && b.left + b.width as i32 <= m.width as i32 + 2, "{c:?} x {b:?}");
            assert!(b.top >= -2 && b.top + b.height as i32 <= m.height as i32 + 2, "{c:?} y {:?}", (b.top, b.height));
        }
        assert!(f.rasterize(' ', Face::Regular, 1).is_empty());
    }

    #[test]
    fn the_subset_kept_what_the_terminal_needs() {
        let f = Fonts::new(12.5, 17);
        for c in ['a', 'é', 'ß', 'Ж', 'λ', '→', '●', '€', '\u{FFFD}', '\u{E0B0}'] {
            assert!(f.covers(c), "{c:?} missing from the embedded subset");
        }
    }

    #[test]
    fn bold_is_heavier_than_regular() {
        let mut f = Fonts::new(12.5, 17);
        let ink = |b: Bitmap| b.data.iter().map(|&v| v as u32).sum::<u32>();
        let r = ink(f.rasterize('H', Face::Regular, 1));
        let b = ink(f.rasterize('H', Face::Bold, 1));
        assert!(b > r * 11 / 10, "bold {b} vs regular {r}");
    }
}
