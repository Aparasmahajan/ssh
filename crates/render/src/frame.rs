//! Grid rows → instanced quads.
//!
//! Each row keeps the quads it produced last time; only rows the caller marks
//! changed are rebuilt (§5.3: "Only changed rows upload"). A frame is then the
//! concatenation, in four layers: every background run, then every glyph and
//! line decoration, then the cursor, then the selection. One draw call (§5.7).
//!
//! Instances are plain pixel rectangles. Cells are whole pixels (see `font`),
//! so every quad is pixel-aligned and the shader fetches atlas texels directly,
//! with no sampler and no filtering to bleed between neighbours.

use bytemuck::{Pod, Zeroable};
use opspilot_core_term::grid::{GRAPHEME_TAG, WIDE_SPACER};
use opspilot_core_term::style::{flags, Style, StyleId, StyleTable};

use crate::atlas::Glyphs;
use crate::font::{CellMetrics, Face};
use crate::palette::{rgba, Palette};

pub const KIND_SOLID: u32 = 0;
pub const KIND_GLYPH: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct Instance {
    /// Top-left, in physical pixels from the surface origin.
    pub pos: [f32; 2],
    pub size: [f32; 2],
    /// Atlas texel of the top-left corner. Unused for solids.
    pub uv: [f32; 2],
    /// RGBA, red in the low byte.
    pub color: u32,
    pub kind: u32,
}

/// A cell position in the viewport.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Point {
    pub row: usize,
    pub col: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CursorDraw {
    pub row: usize,
    pub col: usize,
    /// What is under the cursor, so a block cursor can draw it inverted.
    pub glyph: u32,
    pub style: Style,
    /// Unfocused panes get a hollow box, as every terminal does: it shows where
    /// the cursor is without suggesting that typing goes there.
    pub focused: bool,
}

/// A pixel rectangle on the surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn full(size: (u32, u32)) -> Rect {
        Rect { x: 0, y: 0, w: size.0, h: size.1 }
    }

    /// The smallest rectangle covering both.
    pub fn union(self, o: Rect) -> Rect {
        let (x, y) = (self.x.min(o.x), self.y.min(o.y));
        let r = (self.x + self.w).max(o.x + o.w);
        let b = (self.y + self.h).max(o.y + o.h);
        Rect { x, y, w: r - x, h: b - y }
    }

    /// Clipped to a surface of this size; `None` if nothing is left.
    pub fn clip(self, size: (u32, u32)) -> Option<Rect> {
        let r = (self.x + self.w).min(size.0);
        let b = (self.y + self.h).min(size.1);
        (self.x < r && self.y < b).then(|| Rect { x: self.x, y: self.y, w: r - self.x, h: b - self.y })
    }
}

/// What changed on screen since the last [`Frame::take_damage`].
#[derive(Clone, Copy, Debug, PartialEq)]
enum Damage {
    None,
    Full,
    /// Inclusive row range.
    Rows(usize, usize),
}

#[derive(Default)]
struct RowCache {
    bg: Vec<Instance>,
    fg: Vec<Instance>,
    valid: bool,
}

pub struct Frame {
    pub palette: Palette,
    metrics: CellMetrics,
    /// Inset from the surface edge, physical pixels (the canvas path's PADDING).
    pad: u32,
    rows: Vec<RowCache>,
    cols: usize,
    cursor: Option<CursorDraw>,
    /// Inclusive, in reading order.
    selection: Option<(Point, Point)>,
    epoch: u32,
    out: Vec<Instance>,
    damage: Damage,
}

impl Frame {
    pub fn new(metrics: CellMetrics, palette: Palette, pad: u32) -> Frame {
        Frame {
            palette,
            metrics,
            pad,
            rows: Vec::new(),
            cols: 0,
            cursor: None,
            selection: None,
            epoch: 0,
            out: Vec::new(),
            damage: Damage::Full,
        }
    }

    pub fn metrics(&self) -> CellMetrics {
        self.metrics
    }

    pub fn pad(&self) -> u32 {
        self.pad
    }

    pub fn rows(&self) -> usize {
        self.rows.len()
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    /// How many rows and columns fit in a surface of this size.
    pub fn fit(metrics: CellMetrics, pad: u32, width: u32, height: u32) -> (usize, usize) {
        let inner = |len: u32, cell: u32| (len.saturating_sub(2 * pad) / cell.max(1)).max(1) as usize;
        (inner(height, metrics.height), inner(width, metrics.width))
    }

    /// Change the grid size. Every row is rebuilt on the next frame.
    pub fn resize(&mut self, rows: usize, cols: usize) {
        if rows == self.rows.len() && cols == self.cols {
            return;
        }
        self.rows.resize_with(rows, RowCache::default);
        self.cols = cols;
        self.invalidate();
    }

    pub fn invalidate(&mut self) {
        for r in &mut self.rows {
            r.valid = false;
        }
        self.damage = Damage::Full;
    }

    fn damage_rows(&mut self, lo: usize, hi: usize) {
        self.damage = match self.damage {
            Damage::Full => Damage::Full,
            Damage::None => Damage::Rows(lo, hi),
            Damage::Rows(a, b) => Damage::Rows(a.min(lo), b.max(hi)),
        };
    }

    /// The pixels that changed since the last call, on a surface of `size`,
    /// or `None` if nothing did. Rows are widened by one on each side: a glyph
    /// may overhang its cell a little, and its old ink there must be redrawn.
    /// Full width, so the damage is one band; a keystroke touches one or two
    /// adjacent rows (the edit and the cursor).
    pub fn take_damage(&mut self, size: (u32, u32)) -> Option<Rect> {
        let d = std::mem::replace(&mut self.damage, Damage::None);
        let rows = match d {
            Damage::None => return None,
            Damage::Full => return Rect::full(size).clip(size),
            Damage::Rows(lo, hi) => (lo, hi),
        };
        let h = self.metrics.height;
        // Widened by a row; touching the first or last row takes in the padding.
        let (lo, hi) = (rows.0.saturating_sub(1), rows.1 + 1);
        let y0 = if lo == 0 { 0 } else { self.pad + lo as u32 * h };
        let y1 = if hi + 1 >= self.rows.len() { size.1 } else { self.pad + (hi as u32 + 1) * h };
        Rect { x: 0, y: y0, w: size.0, h: y1.saturating_sub(y0) }.clip(size)
    }

    /// Rebuild this row on the next frame. Out-of-range rows are ignored.
    pub fn invalidate_row(&mut self, row: usize) {
        if let Some(r) = self.rows.get_mut(row) {
            r.valid = false;
        }
    }

    /// Rows that must be passed to [`Frame::set_row`] before the next draw:
    /// never built, invalidated, or built against an atlas that has since been
    /// cleared.
    pub fn stale_rows(&mut self, glyphs: &Glyphs) -> impl Iterator<Item = usize> + '_ {
        if glyphs.epoch != self.epoch {
            self.epoch = glyphs.epoch;
            self.invalidate();
        }
        self.rows.iter().enumerate().filter(|(_, r)| !r.valid).map(|(i, _)| i)
    }

    /// Rebuild one row. `glyphs`/`styles` may be shorter than the grid width
    /// (scrollback rows drop trailing blanks); the rest is default background.
    /// O(cols).
    pub fn set_row(&mut self, atlas: &mut Glyphs, row: usize, glyphs: &[u32], styles: &[StyleId], table: &StyleTable) {
        if row >= self.rows.len() {
            return;
        }
        self.damage_rows(row, row);
        let Some(cache) = self.rows.get_mut(row) else { return };
        cache.bg.clear();
        cache.fg.clear();
        cache.valid = true;
        let m = self.metrics;
        let (cw, ch) = (m.width as f32, m.height as f32);
        let y = (self.pad + row as u32 * m.height) as f32;
        let x_of = |col: usize| (self.pad + col as u32 * m.width) as f32;
        let n = glyphs.len().min(styles.len()).min(self.cols);

        // Background runs: adjacent cells with one colour become one quad.
        let mut run: Option<(usize, u32)> = None;
        let flush = |cache: &mut RowCache, run: Option<(usize, u32)>, end: usize| {
            if let Some((start, color)) = run {
                cache.bg.push(solid(x_of(start), y, (end - start) as f32 * cw, ch, rgba(color, 255)));
            }
        };

        for col in 0..n {
            let style = table.get(styles[col]);
            let (fg, bg) = colors(&self.palette, style);
            let bg_run = (bg != self.palette.bg).then_some(bg);
            if run.map(|r| r.1) != bg_run {
                flush(cache, run, col);
                run = bg_run.map(|c| (col, c));
            }

            let g = glyphs[col];
            if g == WIDE_SPACER {
                continue;
            }
            let wide = glyphs.get(col + 1) == Some(&WIDE_SPACER);
            let span = if wide { 2.0 } else { 1.0 };
            let x = x_of(col);
            let alpha = if style.flags & flags::DIM != 0 { 160 } else { 255 };
            let color = rgba(fg, alpha);

            let visible = style.flags & flags::HIDDEN == 0;
            if visible && g != b' ' as u32 && g != 0 {
                // Grapheme clusters are not interned by the engine yet; one
                // that appears is shown as U+FFFD, not as a random codepoint.
                let cp = if g & GRAPHEME_TAG != 0 { 0xFFFD } else { g };
                let face = Face::new(style.flags & flags::BOLD != 0, style.flags & flags::ITALIC != 0);
                let s = atlas.slot(cp, face, wide);
                if s.w > 0 {
                    cache.fg.push(Instance {
                        pos: [x + s.left as f32, y + s.top as f32],
                        size: [s.w as f32, s.h as f32],
                        uv: [s.x as f32, s.y as f32],
                        color,
                        kind: KIND_GLYPH,
                    });
                }
            }
            if style.flags & flags::UNDERLINE != 0 {
                cache.fg.push(solid(x, y + m.underline as f32, span * cw, m.stroke as f32, color));
            }
            if style.flags & flags::STRIKE != 0 {
                cache.fg.push(solid(x, y + m.strike as f32, span * cw, m.stroke as f32, color));
            }
        }
        flush(cache, run, n);
    }

    pub fn set_cursor(&mut self, cursor: Option<CursorDraw>) {
        if cursor == self.cursor {
            return;
        }
        for c in [self.cursor, cursor].into_iter().flatten() {
            self.damage_rows(c.row, c.row);
        }
        self.cursor = cursor;
    }

    pub fn set_selection(&mut self, selection: Option<(Point, Point)>) {
        let selection = selection.map(|(a, b)| if a <= b { (a, b) } else { (b, a) });
        if selection == self.selection {
            return;
        }
        for (a, b) in [self.selection, selection].into_iter().flatten() {
            self.damage_rows(a.row, b.row);
        }
        self.selection = selection;
    }

    /// The whole frame, back to front. O(instances).
    pub fn instances(&mut self, atlas: &mut Glyphs) -> &[Instance] {
        self.out.clear();
        for r in &self.rows {
            self.out.extend_from_slice(&r.bg);
        }
        for r in &self.rows {
            self.out.extend_from_slice(&r.fg);
        }
        let m = self.metrics;
        let (cw, ch) = (m.width as f32, m.height as f32);
        let cell = |row: usize, col: usize| ((self.pad + col as u32 * m.width) as f32, (self.pad + row as u32 * m.height) as f32);

        if let Some(c) = self.cursor.filter(|c| c.row < self.rows.len() && c.col < self.cols) {
            let (x, y) = cell(c.row, c.col);
            let color = rgba(self.palette.cursor, 255);
            if c.focused {
                self.out.push(solid(x, y, cw.max(1.0), ch, color));
                // The character under a block cursor, in the background colour,
                // so it reads as inverted rather than eaten.
                if c.glyph != b' ' as u32 && c.glyph != 0 && c.glyph != WIDE_SPACER && c.glyph & GRAPHEME_TAG == 0 {
                    let face = Face::new(c.style.flags & flags::BOLD != 0, c.style.flags & flags::ITALIC != 0);
                    let s = atlas.slot(c.glyph, face, false);
                    if s.w > 0 {
                        self.out.push(Instance {
                            pos: [x + s.left as f32, y + s.top as f32],
                            size: [s.w as f32, s.h as f32],
                            uv: [s.x as f32, s.y as f32],
                            color: rgba(self.palette.bg, 255),
                            kind: KIND_GLYPH,
                        });
                    }
                }
            } else {
                let t = m.stroke.max(1) as f32;
                self.out.push(solid(x, y, cw, t, color));
                self.out.push(solid(x, y + ch - t, cw, t, color));
                self.out.push(solid(x, y, t, ch, color));
                self.out.push(solid(x + cw - t, y, t, ch, color));
            }
        }

        if let Some((a, b)) = self.selection {
            let color = rgba(self.palette.selection, self.palette.selection_alpha);
            let last = self.cols.saturating_sub(1);
            // O(selected rows): one quad per row.
            for row in a.row..=b.row.min(self.rows.len().saturating_sub(1)) {
                let c0 = if row == a.row { a.col } else { 0 };
                let c1 = if row == b.row { b.col.min(last) } else { last };
                if c0 > c1 {
                    continue;
                }
                let (x, y) = cell(row, c0);
                self.out.push(solid(x, y, (c1 - c0 + 1) as f32 * cw, ch, color));
            }
        }
        &self.out
    }
}

/// Foreground and background, with reverse video applied.
fn colors(p: &Palette, s: Style) -> (u32, u32) {
    let fg = p.resolve(s.fg, false);
    let bg = p.resolve(s.bg, true);
    if s.flags & flags::REVERSE != 0 {
        (bg, fg)
    } else {
        (fg, bg)
    }
}

fn solid(x: f32, y: f32, w: f32, h: f32, color: u32) -> Instance {
    Instance { pos: [x, y], size: [w, h], uv: [0.0, 0.0], color, kind: KIND_SOLID }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::Fonts;
    use opspilot_core_term::{Parser, Screen};

    fn setup(text: &[u8]) -> (Screen, Glyphs, Frame) {
        let mut screen = Screen::new(4, 20);
        Parser::new().advance(&mut screen, text);
        let glyphs = Glyphs::new(Fonts::new(12.5, 17));
        let mut frame = Frame::new(glyphs.fonts.metrics, Palette::default(), 6);
        frame.resize(4, 20);
        (screen, glyphs, frame)
    }

    fn build(screen: &Screen, glyphs: &mut Glyphs, frame: &mut Frame) {
        let stale: Vec<usize> = frame.stale_rows(glyphs).collect();
        for r in stale {
            let (g, s) = screen.grid().row(r);
            frame.set_row(glyphs, r, g, s, screen.styles());
        }
    }

    #[test]
    fn damage_covers_changed_rows_plus_one_either_side() {
        let (mut screen, mut glyphs, mut frame) = setup(b"one\r\ntwo");
        build(&screen, &mut glyphs, &mut frame);
        let size = (2 * 6 + 20 * 8, 2 * 6 + 4 * 17);
        assert_eq!(frame.take_damage(size), Some(Rect::full(size)), "first frame is full");
        assert_eq!(frame.take_damage(size), None, "nothing changed since");

        Parser::new().advance(&mut screen, b"!");
        frame.invalidate_row(1);
        build(&screen, &mut glyphs, &mut frame);
        // Row 1 widened to rows 0..=2. Row 0 starts at the surface edge.
        assert_eq!(frame.take_damage(size), Some(Rect { x: 0, y: 0, w: size.0, h: 6 + 3 * 17 }));

        frame.invalidate_row(2);
        build(&screen, &mut glyphs, &mut frame);
        // Rows 1..=3; row 3 is the last, so the band runs to the bottom edge.
        assert_eq!(frame.take_damage(size), Some(Rect { x: 0, y: 6 + 17, w: size.0, h: size.1 - 6 - 17 }));
    }

    #[test]
    fn cursor_and_selection_damage_their_old_and_new_rows_only_when_changed() {
        let screen = Screen::new(10, 20);
        let mut glyphs = Glyphs::new(Fonts::new(12.5, 17));
        let mut frame = Frame::new(glyphs.fonts.metrics, Palette::default(), 6);
        frame.resize(10, 20);
        build(&screen, &mut glyphs, &mut frame);
        let size = (2 * 6 + 20 * 8, 2 * 6 + 10 * 17);
        frame.take_damage(size);
        let cur = |row| CursorDraw { row, col: 0, glyph: b' ' as u32, style: Style::default(), focused: true };
        frame.set_cursor(Some(cur(4)));
        frame.take_damage(size);
        frame.set_cursor(Some(cur(4)));
        assert_eq!(frame.take_damage(size), None, "an unchanged cursor is no damage");
        frame.set_cursor(Some(cur(6)));
        let d = frame.take_damage(size).unwrap();
        assert_eq!((d.y, d.y + d.h), (6 + 3 * 17, 6 + 8 * 17), "rows 4 and 6, widened");
        frame.set_selection(Some((Point { row: 2, col: 1 }, Point { row: 3, col: 0 })));
        let d = frame.take_damage(size).unwrap();
        assert_eq!((d.y, d.y + d.h), (6 + 17, 6 + 5 * 17));
        frame.set_selection(Some((Point { row: 2, col: 1 }, Point { row: 3, col: 0 })));
        assert_eq!(frame.take_damage(size), None);
    }

    #[test]
    fn rect_union_and_clip() {
        let a = Rect { x: 0, y: 10, w: 100, h: 20 };
        let b = Rect { x: 0, y: 50, w: 100, h: 5 };
        assert_eq!(a.union(b), Rect { x: 0, y: 10, w: 100, h: 45 });
        assert_eq!(Rect { x: 0, y: 90, w: 100, h: 20 }.clip((100, 100)), Some(Rect { x: 0, y: 90, w: 100, h: 10 }));
        assert_eq!(Rect { x: 0, y: 100, w: 100, h: 20 }.clip((100, 100)), None);
    }

    #[test]
    fn plain_text_is_one_glyph_per_character_and_no_backgrounds() {
        let (screen, mut glyphs, mut frame) = setup(b"hi there");
        build(&screen, &mut glyphs, &mut frame);
        let inst = frame.instances(&mut glyphs);
        assert_eq!(inst.iter().filter(|i| i.kind == KIND_GLYPH).count(), 7, "spaces draw nothing");
        assert_eq!(inst.iter().filter(|i| i.kind == KIND_SOLID).count(), 0, "default background is the clear colour");
    }

    #[test]
    fn a_coloured_background_is_one_run_not_one_quad_per_cell() {
        let (screen, mut glyphs, mut frame) = setup(b"\x1b[41m     \x1b[0m");
        build(&screen, &mut glyphs, &mut frame);
        let bg: Vec<_> = frame.instances(&mut glyphs).iter().filter(|i| i.kind == KIND_SOLID).copied().collect();
        assert_eq!(bg.len(), 1);
        assert_eq!(bg[0].size[0], 5.0 * 8.0);
        assert_eq!(bg[0].color, rgba(0xE5484D, 255));
    }

    #[test]
    fn backgrounds_are_drawn_before_any_glyph() {
        let (screen, mut glyphs, mut frame) = setup(b"ab\r\n\x1b[44mcd");
        build(&screen, &mut glyphs, &mut frame);
        let inst = frame.instances(&mut glyphs);
        let first_glyph = inst.iter().position(|i| i.kind == KIND_GLYPH).unwrap();
        assert!(inst[..first_glyph].iter().all(|i| i.kind == KIND_SOLID));
        assert!(inst[first_glyph..].iter().all(|i| i.kind == KIND_GLYPH));
    }

    #[test]
    fn only_marked_rows_are_rebuilt() {
        let (mut screen, mut glyphs, mut frame) = setup(b"one\r\ntwo");
        build(&screen, &mut glyphs, &mut frame);
        assert_eq!(frame.stale_rows(&glyphs).count(), 0);
        Parser::new().advance(&mut screen, b"\r\nthree");
        frame.rows[2].valid = false;
        assert_eq!(frame.stale_rows(&glyphs).collect::<Vec<_>>(), vec![2]);
        build(&screen, &mut glyphs, &mut frame);
        assert_eq!(frame.instances(&mut glyphs).iter().filter(|i| i.kind == KIND_GLYPH).count(), 11);
    }

    #[test]
    fn a_wide_character_takes_one_glyph_and_the_spacer_none() {
        let (screen, mut glyphs, mut frame) = setup("日x".as_bytes());
        build(&screen, &mut glyphs, &mut frame);
        let g: Vec<_> = frame.instances(&mut glyphs).iter().filter(|i| i.kind == KIND_GLYPH).copied().collect();
        assert_eq!(g.len(), 2);
        // `x` starts in column 2, after the wide cell.
        assert!(g[1].pos[0] >= (6 + 2 * 8) as f32 - 2.0, "{:?}", g[1]);
    }

    #[test]
    fn underline_draws_under_spaces_too() {
        let (screen, mut glyphs, mut frame) = setup(b"\x1b[4m  \x1b[0m");
        build(&screen, &mut glyphs, &mut frame);
        let lines = frame.instances(&mut glyphs).iter().filter(|i| i.kind == KIND_SOLID).count();
        assert_eq!(lines, 2);
    }

    #[test]
    fn cursor_and_selection_come_last() {
        let (screen, mut glyphs, mut frame) = setup(b"abc");
        build(&screen, &mut glyphs, &mut frame);
        let (g, s) = screen.grid().row(0);
        frame.set_cursor(Some(CursorDraw {
            row: 0,
            col: 1,
            glyph: g[1],
            style: screen.styles().get(s[1]),
            focused: true,
        }));
        frame.set_selection(Some((Point { row: 1, col: 3 }, Point { row: 0, col: 2 })));
        let inst = frame.instances(&mut glyphs).to_vec();
        let n = inst.len();
        // block, inverted `b`, then two selection rows (reversed input is reordered).
        assert_eq!(inst[n - 4].color, rgba(Palette::default().cursor, 255));
        assert_eq!(inst[n - 3].kind, KIND_GLYPH);
        assert_eq!(inst[n - 2].pos[1], 6.0, "selection starts on row 0");
        assert_eq!(inst[n - 2].size[0], (20 - 2) as f32 * 8.0, "row 0 from col 2 to the end");
        assert_eq!(inst[n - 1].size[0], 4.0 * 8.0, "row 1 up to col 3");
    }

    #[test]
    fn fit_matches_the_canvas_arithmetic() {
        let m = Fonts::new(12.5, 17).metrics;
        // (h - 2*pad) / cell_h, (w - 2*pad) / cell_w.
        assert_eq!(Frame::fit(m, 6, 812, 352), (20, 100));
    }
}
