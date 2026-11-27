//! Box drawing and block elements, drawn to fill the cell exactly.
//!
//! The font's own box glyphs span the em box. The cell is taller than that
//! (line height 1.36), so a vertical line from the font leaves a gap at every
//! row boundary and `tree`, `systemctl status` and every TUI border come out
//! dashed. Drawn here, a line reaches the cell edge and meets its neighbour.
//!
//! Covered: light and heavy lines with every mixed junction (U+2500–U+254B,
//! less the dashed ones), the half lines (U+2574–U+257F), rounded corners as
//! square ones, `═` and `║`, and all of Block Elements (U+2580–U+259F). The
//! other double-line junctions and the dashes come from the font, which is a
//! gap at row boundaries but not a wrong shape.

use crate::font::{Bitmap, CellMetrics, Face};

/// Arm weights, up/down/left/right: 0 none, 1 light, 2 heavy.
type Arms = [u8; 4];

/// U+2500..=U+254B. `[0; 4]` = not drawn here (the dashed lines).
#[rustfmt::skip]
const LINES: [Arms; 76] = [
    [0,0,1,1], [0,0,2,2], [1,1,0,0], [2,2,0,0],                         // ─ ━ │ ┃
    [0;4], [0;4], [0;4], [0;4], [0;4], [0;4], [0;4], [0;4],             // dashes
    [0,1,0,1], [0,1,0,2], [0,2,0,1], [0,2,0,2],                         // ┌ ┍ ┎ ┏
    [0,1,1,0], [0,1,2,0], [0,2,1,0], [0,2,2,0],                         // ┐ ┑ ┒ ┓
    [1,0,0,1], [1,0,0,2], [2,0,0,1], [2,0,0,2],                         // └ ┕ ┖ ┗
    [1,0,1,0], [1,0,2,0], [2,0,1,0], [2,0,2,0],                         // ┘ ┙ ┚ ┛
    [1,1,0,1], [1,1,0,2], [2,1,0,1], [1,2,0,1],                         // ├ ┝ ┞ ┟
    [2,2,0,1], [2,1,0,2], [1,2,0,2], [2,2,0,2],                         // ┠ ┡ ┢ ┣
    [1,1,1,0], [1,1,2,0], [2,1,1,0], [1,2,1,0],                         // ┤ ┥ ┦ ┧
    [2,2,1,0], [2,1,2,0], [1,2,2,0], [2,2,2,0],                         // ┨ ┩ ┪ ┫
    [0,1,1,1], [0,1,2,1], [0,1,1,2], [0,1,2,2],                         // ┬ ┭ ┮ ┯
    [0,2,1,1], [0,2,2,1], [0,2,1,2], [0,2,2,2],                         // ┰ ┱ ┲ ┳
    [1,0,1,1], [1,0,2,1], [1,0,1,2], [1,0,2,2],                         // ┴ ┵ ┶ ┷
    [2,0,1,1], [2,0,2,1], [2,0,1,2], [2,0,2,2],                         // ┸ ┹ ┺ ┻
    [1,1,1,1], [1,1,2,1], [1,1,1,2], [1,1,2,2],                         // ┼ ┽ ┾ ┿
    [2,1,1,1], [1,2,1,1], [2,2,1,1], [2,1,2,1],                         // ╀ ╁ ╂ ╃
    [2,1,1,2], [1,2,2,1], [1,2,1,2], [2,1,2,2],                         // ╄ ╅ ╆ ╇
    [1,2,2,2], [2,2,2,1], [2,2,1,2], [2,2,2,2],                         // ╈ ╉ ╊ ╋
];

/// U+2574..=U+257F: half lines and the mixed light/heavy straight lines.
#[rustfmt::skip]
const HALF: [Arms; 12] = [
    [0,0,1,0], [1,0,0,0], [0,0,0,1], [0,1,0,0],                         // ╴ ╵ ╶ ╷
    [0,0,2,0], [2,0,0,0], [0,0,0,2], [0,2,0,0],                         // ╸ ╹ ╺ ╻
    [0,0,1,2], [1,2,0,0], [0,0,2,1], [2,1,0,0],                         // ╼ ╽ ╾ ╿
];

fn arms(c: u32) -> Option<Arms> {
    let a = match c {
        0x2500..=0x254B => LINES[(c - 0x2500) as usize],
        0x2574..=0x257F => HALF[(c - 0x2574) as usize],
        // Rounded corners, drawn square.
        0x256D => [0, 1, 0, 1],
        0x256E => [0, 1, 1, 0],
        0x256F => [1, 0, 1, 0],
        0x2570 => [1, 0, 0, 1],
        _ => return None,
    };
    (a != [0; 4]).then_some(a)
}

struct Canvas {
    w: u32,
    h: u32,
    data: Vec<u8>,
}

impl Canvas {
    fn new(m: CellMetrics) -> Canvas {
        Canvas { w: m.width, h: m.height, data: vec![0; (m.width * m.height) as usize] }
    }

    /// Fill `[x0, x1) × [y0, y1)`, clamped to the cell.
    fn fill(&mut self, x0: u32, y0: u32, x1: u32, y1: u32, a: u8) {
        for y in y0.min(self.h)..y1.min(self.h) {
            let row = (y * self.w) as usize;
            self.data[row + x0.min(self.w) as usize..row + x1.min(self.w) as usize].fill(a);
        }
    }

    fn done(self) -> Bitmap {
        Bitmap { left: 0, top: 0, width: self.w, height: self.h, data: self.data }
    }
}

pub fn draw(ch: char, m: CellMetrics, face: Face) -> Option<Bitmap> {
    let c = ch as u32;
    if !(0x2500..=0x259F).contains(&c) {
        return None;
    }
    let bold = matches!(face, Face::Bold | Face::BoldItalic);
    let light = m.stroke.max(1) + u32::from(bold);
    let heavy = light * 2;
    let t = |weight: u8| if weight == 2 { heavy } else { light };
    let mut cv = Canvas::new(m);
    let (w, h) = (m.width, m.height);

    if let Some([u, d, l, r]) = arms(c) {
        // The stroke at the junction is as thick as the thickest arm crossing
        // it, so a light arm meeting a heavy one runs into it with no notch.
        let tv = if u | d == 0 { 0 } else { t(u.max(d)) };
        let th = if l | r == 0 { 0 } else { t(l.max(r)) };
        let (vx0, hy0) = ((w - tv.min(w)) / 2, (h - th.min(h)) / 2);
        // Horizontal arms reach the far side of the vertical stroke and vice
        // versa; with no crossing stroke they meet at the centre.
        let (jx0, jx1) = if tv > 0 { (vx0, vx0 + tv) } else { (w / 2, w / 2) };
        let (jy0, jy1) = if th > 0 { (hy0, hy0 + th) } else { (h / 2, h / 2) };
        let span = |len: u32, s: u32| ((len - s.min(len)) / 2, (len - s.min(len)) / 2 + s);
        if l > 0 {
            let (y0, y1) = span(h, t(l));
            cv.fill(0, y0, jx1, y1, 255);
        }
        if r > 0 {
            let (y0, y1) = span(h, t(r));
            cv.fill(jx0, y0, w, y1, 255);
        }
        if u > 0 {
            let (x0, x1) = span(w, t(u));
            cv.fill(x0, 0, x1, jy1, 255);
        }
        if d > 0 {
            let (x0, x1) = span(w, t(d));
            cv.fill(x0, jy0, x1, h, 255);
        }
        return Some(cv.done());
    }

    // Eighths of the cell, rounded, so adjacent partial blocks tile exactly.
    let ex = |n: u32| (w * n + 4) / 8;
    let ey = |n: u32| (h * n + 4) / 8;
    let (hw, hh) = (w / 2, h / 2);
    match c {
        // ═ ║: two light strokes with a light-width gap.
        0x2550 => {
            let y0 = (h - 3 * light.min(h / 3)) / 2;
            cv.fill(0, y0, w, y0 + light, 255);
            cv.fill(0, y0 + 2 * light, w, y0 + 3 * light, 255);
        }
        0x2551 => {
            let x0 = (w - 3 * light.min(w / 3)) / 2;
            cv.fill(x0, 0, x0 + light, h, 255);
            cv.fill(x0 + 2 * light, 0, x0 + 3 * light, h, 255);
        }
        0x2580 => cv.fill(0, 0, w, hh, 255),
        0x2581..=0x2588 => cv.fill(0, h - ey(c - 0x2580), w, h, 255),
        0x2589..=0x258F => cv.fill(0, 0, ex(0x2590 - c), h, 255),
        0x2590 => cv.fill(hw, 0, w, h, 255),
        // Shades as flat coverage rather than a dither: at 8 px a dither
        // pattern is a moiré, and the fill colour still comes from the cell.
        0x2591 => cv.fill(0, 0, w, h, 64),
        0x2592 => cv.fill(0, 0, w, h, 128),
        0x2593 => cv.fill(0, 0, w, h, 192),
        0x2594 => cv.fill(0, 0, w, ey(1), 255),
        0x2595 => cv.fill(w - ex(1), 0, w, h, 255),
        0x2596..=0x259F => {
            // Quadrant bits: upper-left, upper-right, lower-left, lower-right.
            const Q: [u8; 10] = [0b0010, 0b0001, 0b1000, 0b1011, 0b1001, 0b1110, 0b1101, 0b0100, 0b0110, 0b0111];
            let q = Q[(c - 0x2596) as usize];
            if q & 0b1000 != 0 {
                cv.fill(0, 0, hw, hh, 255);
            }
            if q & 0b0100 != 0 {
                cv.fill(hw, 0, w, hh, 255);
            }
            if q & 0b0010 != 0 {
                cv.fill(0, hh, hw, h, 255);
            }
            if q & 0b0001 != 0 {
                cv.fill(hw, hh, w, h, 255);
            }
        }
        _ => return None,
    }
    Some(cv.done())
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: CellMetrics = CellMetrics { width: 8, height: 17, baseline: 13, underline: 15, stroke: 1, strike: 9 };

    fn at(b: &Bitmap, x: u32, y: u32) -> u8 {
        b.data[(y * b.width + x) as usize]
    }

    #[test]
    fn a_vertical_line_reaches_both_edges() {
        let b = draw('│', M, Face::Regular).unwrap();
        let col = (0..8).find(|&x| at(&b, x, 0) == 255).expect("ink on the top row");
        assert_eq!(at(&b, col, 16), 255, "ink on the bottom row, same column");
    }

    #[test]
    fn junctions_connect_to_their_neighbours() {
        // ├ then ─: the right arm of ├ must sit on the same rows as ─.
        let tee = draw('├', M, Face::Regular).unwrap();
        let dash = draw('─', M, Face::Regular).unwrap();
        let rows = |b: &Bitmap, x: u32| (0..17).filter(|&y| at(b, x, y) > 0).collect::<Vec<_>>();
        assert_eq!(rows(&tee, 7), rows(&dash, 0));
        // └ has no down arm: nothing on the bottom row.
        let corner = draw('└', M, Face::Regular).unwrap();
        assert!((0..8).all(|x| at(&corner, x, 16) == 0));
    }

    #[test]
    fn heavy_is_thicker_than_light() {
        let ink = |c| draw(c, M, Face::Regular).unwrap().data.iter().filter(|&&v| v > 0).count();
        assert_eq!(ink('━'), 2 * ink('─'));
    }

    #[test]
    fn full_block_fills_the_cell_and_halves_tile() {
        let full = draw('█', M, Face::Regular).unwrap();
        assert!(full.data.iter().all(|&v| v == 255));
        let top = draw('▀', M, Face::Regular).unwrap();
        let bottom = draw('▄', M, Face::Regular).unwrap();
        for (a, b) in top.data.iter().zip(&bottom.data) {
            assert_eq!(a.saturating_add(*b), 255, "▀ and ▄ must tile with no gap or overlap");
        }
    }

    #[test]
    fn every_table_entry_draws_something() {
        for c in (0x2500..=0x254Bu32).chain(0x2574..=0x257F).chain(0x2580..=0x259F) {
            let ch = char::from_u32(c).unwrap();
            let dashed = (0x2504..=0x250B).contains(&c);
            match draw(ch, M, Face::Regular) {
                Some(b) => assert!(b.data.iter().any(|&v| v > 0), "{ch} drew nothing"),
                None => assert!(dashed, "{ch} not drawn"),
            }
        }
    }
}
