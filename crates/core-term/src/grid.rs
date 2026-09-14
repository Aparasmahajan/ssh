//! The cell grid — structure-of-arrays over a row ring, with an O(1) damage bitset.
//!
//! CLAUDE.md §5.1 and §5.3. Three rules, all load-bearing:
//!
//!   · "Never `Vec<Vec<Cell>>`. That is a pointer chase per row and a real cause
//!     of missed frames." The planes are flat and typed.
//!   · Parallel planes, so a pass needing only glyphs streams contiguous memory
//!     and never touches the style plane.
//!   · 6 bytes per cell, via style interning.
//!
//! ## Why there is a row offset
//!
//! The first version scrolled with `copy_within` over the whole plane. It was
//! correct and it was 53x too slow: `benches/term.rs` measured the parser at
//! 400 MiB/s standalone but only 7.45 MiB/s once the grid was attached. Cause,
//! from the arithmetic rather than a guess — a 4 MiB `cat` is ~75,000 line feeds,
//! each memmoving a 50x200 grid (60 KB), so 4.5 GB of memmove to display 4 MiB.
//!
//! So the viewport is a **ring of rows**: logical row `r` lives at physical row
//! `(r + row_offset) % rows`. Scrolling increments the offset and clears one row
//! — O(cols) instead of O(rows*cols). Row contents stay contiguous, so the SIMD
//! stride within a row is unchanged and there is still no pointer chase; the only
//! cost is one add and one predictable compare per row lookup.
//!
//! CLAUDE.md §1: "If a path is slow, ask whether the *protocol or architecture*
//! is wrong before tuning the loop."

use crate::style::{StyleId, DEFAULT_STYLE};

/// Second half of a double-width cell (§5.1: "Wide (CJK/emoji) cells occupy two
/// slots; the second carries `WIDE_SPACER`"). Not a valid codepoint, so it can
/// never collide with real content.
pub const WIDE_SPACER: u32 = 0xFFFF_FFFF;

/// Set on `glyph` when the value is an interned grapheme-cluster id rather than
/// a bare codepoint (§5.1).
pub const GRAPHEME_TAG: u32 = 0x8000_0000;

pub const BLANK: u32 = b' ' as u32;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    pub row: usize,
    pub col: usize,
}

#[derive(Debug)]
pub struct Grid {
    rows: usize,
    cols: usize,
    /// Codepoint, `WIDE_SPACER`, or a `GRAPHEME_TAG`-marked id. len = rows*cols.
    glyph: Vec<u32>,
    /// Index into the screen's `StyleTable`. len = rows*cols.
    style: Vec<StyleId>,
    /// Physical row holding logical row 0. Always `< rows`.
    row_offset: usize,
    /// §5.3: `dirty_rows: [u64; ceil(rows/64)]`, indexed by LOGICAL row.
    dirty: Vec<u64>,
    /// Per logical row: does this line continue onto the next?
    ///
    /// Reflow is impossible without it. A row that filled up and auto-wrapped
    /// and a row that ended in a newline look identical in the cell planes, and
    /// re-wrapping the first while joining the second is the difference between
    /// resizing correctly and mangling the screen.
    wrapped: Vec<bool>,
    pub cursor: Cursor,
}

impl Grid {
    pub fn new(rows: usize, cols: usize) -> Self {
        let cells = rows * cols;
        let mut g = Grid {
            rows,
            cols,
            glyph: vec![BLANK; cells],
            style: vec![DEFAULT_STYLE; cells],
            row_offset: 0,
            dirty: vec![0; dirty_words(rows)],
            wrapped: vec![false; rows],
            cursor: Cursor::default(),
        };
        g.mark_all_dirty();
        g
    }

    #[inline]
    pub fn rows(&self) -> usize {
        self.rows
    }

    #[inline]
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// Logical row -> physical row. One add and one conditional subtract; both
    /// operands are `< rows`, so no division.
    #[inline]
    fn phys(&self, row: usize) -> usize {
        let r = row + self.row_offset;
        if r >= self.rows {
            r - self.rows
        } else {
            r
        }
    }

    #[inline]
    fn row_start(&self, row: usize) -> usize {
        self.phys(row) * self.cols
    }

    /// Flat index of a logical cell. Use for a single lookup; prefer
    /// [`Grid::row`] when touching a whole row.
    #[inline]
    pub fn idx(&self, row: usize, col: usize) -> usize {
        self.row_start(row) + col
    }

    #[inline]
    pub fn cell(&self, row: usize, col: usize) -> (u32, StyleId) {
        let i = self.idx(row, col);
        (self.glyph[i], self.style[i])
    }

    /// Both planes for one logical row, contiguous.
    ///
    /// This is what the renderer and the scrollback read. There is deliberately
    /// no accessor for the whole plane: with the ring offset, the raw `Vec` is
    /// not in logical order, and handing it out would invite a caller to iterate
    /// it as if it were.
    #[inline]
    pub fn row(&self, row: usize) -> (&[u32], &[StyleId]) {
        let s = self.row_start(row);
        let e = s + self.cols;
        (&self.glyph[s..e], &self.style[s..e])
    }

    /* ---- Damage (§5.3) --------------------------------------------------- */

    /// O(1), branchless.
    #[inline]
    pub fn mark_dirty(&mut self, row: usize) {
        if row < self.rows {
            self.dirty[row >> 6] |= 1u64 << (row & 63);
        }
    }

    pub fn mark_all_dirty(&mut self) {
        for w in &mut self.dirty {
            *w = u64::MAX;
        }
        // Clear bits past the last real row so `dirty_rows` never yields one —
        // the renderer would upload past the end of the grid.
        let excess = dirty_words(self.rows) * 64 - self.rows;
        if excess > 0 {
            if let Some(last) = self.dirty.last_mut() {
                *last >>= excess;
            }
        }
    }

    pub fn clear_damage(&mut self) {
        for w in &mut self.dirty {
            *w = 0;
        }
    }

    pub fn is_dirty(&self, row: usize) -> bool {
        row < self.rows && self.dirty[row >> 6] & (1u64 << (row & 63)) != 0
    }

    pub fn any_dirty(&self) -> bool {
        self.dirty.iter().any(|&w| w != 0)
    }

    /// Dirty logical row indices, ascending.
    ///
    /// §5.3: "The renderer scans words and skips zero words via
    /// `trailing_zeros()`." A clean screen costs one comparison per 64 rows.
    pub fn dirty_rows(&self) -> DirtyRows<'_> {
        DirtyRows {
            dirty: &self.dirty,
            word: 0,
            current: if self.dirty.is_empty() {
                0
            } else {
                self.dirty[0]
            },
        }
    }

    /// Mark that `row` continues onto the next line (auto-wrap, not a newline).
    #[inline]
    pub fn set_wrapped(&mut self, row: usize, value: bool) {
        if let Some(slot) = self.wrapped.get_mut(row) {
            *slot = value;
        }
    }

    #[inline]
    pub fn is_wrapped(&self, row: usize) -> bool {
        self.wrapped.get(row).copied().unwrap_or(false)
    }

    /* ---- Mutation -------------------------------------------------------- */

    #[inline]
    pub fn set(&mut self, row: usize, col: usize, glyph: u32, style: StyleId) {
        if row >= self.rows || col >= self.cols {
            return;
        }
        let i = self.row_start(row) + col;
        self.glyph[i] = glyph;
        self.style[i] = style;
        self.dirty[row >> 6] |= 1u64 << (row & 63);
    }

    /// Bulk-write a run of printable ASCII into one row.
    ///
    /// The §5.2 fast path's landing site: "bulk-write glyph and style planes in
    /// a tight vectorized loop". Both planes are contiguous slices, so each loop
    /// is straight-line and widenable. One damage mark for the whole run.
    ///
    /// Returns cells written (clipped at the row end).
    pub fn set_ascii_run(&mut self, row: usize, col: usize, bytes: &[u8], style: StyleId) -> usize {
        if row >= self.rows || col >= self.cols {
            return 0;
        }
        let n = bytes.len().min(self.cols - col);
        let start = self.row_start(row) + col;

        for (g, &b) in self.glyph[start..start + n].iter_mut().zip(&bytes[..n]) {
            *g = b as u32;
        }
        self.style[start..start + n].fill(style);
        self.dirty[row >> 6] |= 1u64 << (row & 63);
        n
    }

    pub fn clear_row(&mut self, row: usize, style: StyleId) {
        if row >= self.rows {
            return;
        }
        let s = self.row_start(row);
        self.glyph[s..s + self.cols].fill(BLANK);
        self.style[s..s + self.cols].fill(style);
        self.mark_dirty(row);
    }

    /// Clear from `col` to the end of `row` (CSI K).
    pub fn clear_row_from(&mut self, row: usize, col: usize, style: StyleId) {
        if row >= self.rows || col >= self.cols {
            return;
        }
        let s = self.row_start(row) + col;
        let e = self.row_start(row) + self.cols;
        self.glyph[s..e].fill(BLANK);
        self.style[s..e].fill(style);
        self.mark_dirty(row);
    }

    pub fn clear_all(&mut self, style: StyleId) {
        self.glyph.fill(BLANK);
        self.style.fill(style);
        self.mark_all_dirty();
    }

    /// Scroll up one line: logical row 0 falls off the top.
    ///
    /// O(cols), not O(rows*cols) — the whole reason `row_offset` exists. Read
    /// [`Grid::row(0)`] first if the outgoing line is wanted for scrollback;
    /// after this returns it is gone.
    pub fn scroll_up(&mut self, style: StyleId) {
        if self.rows == 0 {
            return;
        }
        self.row_offset = if self.row_offset + 1 >= self.rows {
            0
        } else {
            self.row_offset + 1
        };
        // The row that just left the top is now the bottom; blank it.
        let last = self.rows - 1;
        let s = self.row_start(last);
        self.glyph[s..s + self.cols].fill(BLANK);
        self.style[s..s + self.cols].fill(style);

        // The flags travel with their rows: logical row r now shows what r+1
        // showed. Forgetting this makes reflow join the wrong lines after any
        // scrolling, which is to say after about a second of real output.
        self.wrapped.rotate_left(1);
        if let Some(last) = self.wrapped.last_mut() {
            *last = false;
        }

        // Every logical row now shows different content.
        self.mark_all_dirty();
    }

    /// Resize, re-wrapping logical lines to the new width.
    ///
    /// §5.6 asks for exactly this: "reflow the visible viewport synchronously
    /// (bounded, ~50 rows), then reflow scrollback lazily on scroll in 1,000-row
    /// chunks on a background task." The viewport half is here; scrollback
    /// reflow is not, and is listed in COMPLETED.md.
    ///
    /// Without it, narrowing the window truncated every visible line — the text
    /// was simply gone, which is what the previous implementation did.
    ///
    /// Complexity: O(rows x cols), bounded by the viewport, never by scrollback.
    pub fn resize(&mut self, rows: usize, cols: usize) {
        if rows == self.rows && cols == self.cols {
            return;
        }
        if cols == self.cols {
            // Height-only: no re-wrapping needed, just a different window onto
            // the same lines.
            self.resize_rows_only(rows);
            return;
        }

        // 1. Gather logical lines: runs of rows joined across `wrapped`.
        let mut logical: Vec<(Vec<u32>, Vec<StyleId>)> = Vec::new();
        let mut cursor_at: Option<(usize, usize)> = None; // (line index, column in line)

        let mut r = 0;
        while r < self.rows {
            let mut glyph: Vec<u32> = Vec::new();
            let mut style: Vec<StyleId> = Vec::new();
            let line_index = logical.len();

            loop {
                let (g, st) = self.row(r);
                if self.cursor.row == r {
                    cursor_at = Some((line_index, glyph.len() + self.cursor.col));
                }
                glyph.extend_from_slice(g);
                style.extend_from_slice(st);
                if self.is_wrapped(r) && r + 1 < self.rows {
                    r += 1;
                } else {
                    break;
                }
            }

            // Trailing blanks are padding, not content. Keeping them would make
            // every short line re-wrap into a full-width one.
            let mut end = glyph.len();
            while end > 0 && glyph[end - 1] == BLANK && style[end - 1] == DEFAULT_STYLE {
                end -= 1;
            }
            glyph.truncate(end);
            style.truncate(end);
            logical.push((glyph, style));
            r += 1;
        }

        // Trailing blank lines below the cursor are not content. Keeping them
        // would push real text off the top when the screen is mostly empty,
        // because step 3 keeps the LAST `rows` lines. The cursor's own line is
        // always kept: that is where the next output goes.
        let mut last_useful = cursor_at.map(|(l, _)| l).unwrap_or(0);
        for (i, (g, _)) in logical.iter().enumerate() {
            if !g.is_empty() {
                last_useful = last_useful.max(i);
            }
        }
        logical.truncate(last_useful + 1);

        // 2. Re-split at the new width.
        let mut out: Vec<(Vec<u32>, Vec<StyleId>, bool)> = Vec::new();
        let mut new_cursor = Cursor::default();

        for (li, (glyph, style)) in logical.iter().enumerate() {
            let chunks = glyph.len().div_ceil(cols).max(1);
            let first_out_row = out.len();
            for c in 0..chunks {
                let lo = c * cols;
                let hi = ((c + 1) * cols).min(glyph.len());
                let mut g = vec![BLANK; cols];
                let mut st = vec![DEFAULT_STYLE; cols];
                if lo < hi {
                    g[..hi - lo].copy_from_slice(&glyph[lo..hi]);
                    st[..hi - lo].copy_from_slice(&style[lo..hi]);
                }
                out.push((g, st, c + 1 < chunks));
            }
            if let Some((cl, ccol)) = cursor_at {
                if cl == li {
                    new_cursor = Cursor {
                        row: first_out_row + (ccol / cols).min(chunks - 1),
                        col: ccol % cols,
                    };
                }
            }
        }

        // 3. Keep the LAST `rows` lines. The bottom of a terminal is where the
        // prompt is, and losing it on resize is the failure people notice.
        let keep_from = out.len().saturating_sub(rows);
        let mut glyph = vec![BLANK; rows * cols];
        let mut style = vec![DEFAULT_STYLE; rows * cols];
        let mut wrapped = vec![false; rows];
        for (i, src) in (keep_from..out.len()).enumerate() {
            if i >= rows {
                break;
            }
            let (g, st, w) = &out[src];
            glyph[i * cols..(i + 1) * cols].copy_from_slice(g);
            style[i * cols..(i + 1) * cols].copy_from_slice(st);
            wrapped[i] = *w;
        }

        self.glyph = glyph;
        self.style = style;
        self.wrapped = wrapped;
        self.rows = rows;
        self.cols = cols;
        self.row_offset = 0;
        self.dirty = vec![0; dirty_words(rows)];
        self.cursor = Cursor {
            row: new_cursor
                .row
                .saturating_sub(keep_from)
                .min(rows.saturating_sub(1)),
            col: new_cursor.col.min(cols.saturating_sub(1)),
        };
        self.mark_all_dirty();
    }

    /// Height-only resize: no re-wrapping, keep the bottom of the screen.
    fn resize_rows_only(&mut self, rows: usize) {
        let cols = self.cols;
        let mut glyph = vec![BLANK; rows * cols];
        let mut style = vec![DEFAULT_STYLE; rows * cols];
        let mut wrapped = vec![false; rows];

        let keep = rows.min(self.rows);
        let src_from = self.rows - keep;
        for i in 0..keep {
            let (g, st) = self.row(src_from + i);
            glyph[i * cols..(i + 1) * cols].copy_from_slice(g);
            style[i * cols..(i + 1) * cols].copy_from_slice(st);
            wrapped[i] = self.is_wrapped(src_from + i);
        }

        let cursor_row = self.cursor.row.saturating_sub(src_from);
        self.glyph = glyph;
        self.style = style;
        self.wrapped = wrapped;
        self.rows = rows;
        self.row_offset = 0;
        self.dirty = vec![0; dirty_words(rows)];
        self.cursor.row = cursor_row.min(rows.saturating_sub(1));
        self.mark_all_dirty();
    }
}

#[inline]
const fn dirty_words(rows: usize) -> usize {
    rows.div_ceil(64)
}

pub struct DirtyRows<'a> {
    dirty: &'a [u64],
    word: usize,
    current: u64,
}

impl Iterator for DirtyRows<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        loop {
            if self.current != 0 {
                let bit = self.current.trailing_zeros() as usize;
                self.current &= self.current - 1; // clear lowest set bit
                return Some(self.word * 64 + bit);
            }
            self.word += 1;
            if self.word >= self.dirty.len() {
                return None;
            }
            self.current = self.dirty[self.word];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(g: &Grid, row: usize) -> String {
        g.row(row)
            .0
            .iter()
            .map(|&c| char::from_u32(c).unwrap_or('?'))
            .collect()
    }

    #[test]
    fn cells_are_six_bytes_per_cell() {
        // §5.1's claim: 6 bytes/cell, not 16. Guarded, because widening StyleId
        // to u32 would silently cost 33% of the GPU upload bandwidth.
        assert_eq!(
            std::mem::size_of::<u32>() + std::mem::size_of::<StyleId>(),
            6
        );
    }

    #[test]
    fn damage_is_tracked_per_row() {
        let mut g = Grid::new(100, 80);
        g.clear_damage();
        assert!(!g.any_dirty());

        g.set(3, 5, b'x' as u32, DEFAULT_STYLE);
        g.set(70, 0, b'y' as u32, DEFAULT_STYLE);

        let rows: Vec<usize> = g.dirty_rows().collect();
        assert_eq!(rows, vec![3, 70]);
    }

    #[test]
    fn mark_all_dirty_yields_exactly_the_real_rows() {
        // rows not a multiple of 64: the tail word has bits with no row behind
        // them, and yielding those would upload past the end of the grid.
        let g = Grid::new(100, 10);
        let rows: Vec<usize> = g.dirty_rows().collect();
        assert_eq!(rows.len(), 100);
        assert_eq!(*rows.last().unwrap(), 99);
    }

    #[test]
    fn ascii_run_clips_at_row_end() {
        let mut g = Grid::new(2, 8);
        let n = g.set_ascii_run(0, 5, b"abcdef", DEFAULT_STYLE);
        assert_eq!(n, 3);
        assert_eq!(g.cell(0, 5).0, b'a' as u32);
        assert_eq!(g.cell(0, 7).0, b'c' as u32);
        assert_eq!(g.cell(1, 0).0, BLANK, "must not wrap into the next row");
    }

    #[test]
    fn scroll_up_shifts_logical_rows_and_blanks_the_bottom() {
        let mut g = Grid::new(3, 4);
        g.set_ascii_run(0, 0, b"aaaa", DEFAULT_STYLE);
        g.set_ascii_run(1, 0, b"bbbb", DEFAULT_STYLE);
        g.set_ascii_run(2, 0, b"cccc", DEFAULT_STYLE);

        g.scroll_up(DEFAULT_STYLE);
        assert_eq!(text(&g, 0), "bbbb");
        assert_eq!(text(&g, 1), "cccc");
        assert_eq!(text(&g, 2), "    ");
    }

    #[test]
    fn scrolling_a_full_cycle_keeps_logical_order() {
        // The ring must stay correct after the offset wraps past `rows`.
        let mut g = Grid::new(3, 2);
        for i in 0..10u8 {
            g.set_ascii_run(2, 0, &[b'0' + i, b'0' + i], DEFAULT_STYLE);
            g.scroll_up(DEFAULT_STYLE);
        }
        // Last three writes were 7,8,9 — then one more scroll blanked the bottom.
        assert_eq!(text(&g, 0), "88");
        assert_eq!(text(&g, 1), "99");
        assert_eq!(text(&g, 2), "  ");
    }

    #[test]
    fn row_zero_is_readable_before_scrolling_it_away() {
        // Screen::newline depends on this to feed scrollback.
        let mut g = Grid::new(2, 3);
        g.set_ascii_run(0, 0, b"top", DEFAULT_STYLE);
        let captured: String = g
            .row(0)
            .0
            .iter()
            .map(|&c| char::from_u32(c).unwrap())
            .collect();
        g.scroll_up(DEFAULT_STYLE);
        assert_eq!(captured, "top");
        assert_eq!(text(&g, 0), "   ");
    }

    #[test]
    fn resize_keeps_the_bottom_of_the_screen() {
        // CONTRACT CHANGE. This previously asserted "preserves top-left", which
        // matched the old truncating resize. A terminal must keep the BOTTOM:
        // that is where the prompt and the cursor are, and dropping them on a
        // window resize is the failure a user actually notices.
        let mut g = Grid::new(4, 10);
        g.set_ascii_run(0, 0, b"first", DEFAULT_STYLE);
        g.set_ascii_run(3, 0, b"last", DEFAULT_STYLE);
        g.cursor = Cursor { row: 3, col: 4 };

        g.resize(2, 10);
        assert_eq!(g.rows(), 2);
        assert_eq!(text(&g, 1).trim_end(), "last");
        assert_eq!(g.cursor.row, 1, "the cursor must stay on screen");
    }

    #[test]
    fn narrowing_reflows_instead_of_truncating() {
        // The bug this replaced: shrinking the window simply cut every line off
        // at the new width and the text was gone.
        let mut g = Grid::new(4, 10);
        g.set_ascii_run(0, 0, b"abcdefghij", DEFAULT_STYLE);
        g.set_wrapped(0, false);

        g.resize(4, 5);
        assert_eq!(text(&g, 0), "abcde");
        assert_eq!(text(&g, 1), "fghij", "the tail must wrap, not vanish");
        assert!(
            g.is_wrapped(0),
            "the first half must be marked as continuing"
        );
        assert!(!g.is_wrapped(1), "the second half ends the logical line");
    }

    #[test]
    fn widening_rejoins_a_wrapped_line() {
        // Two rows that were one logical line must become one row again, or
        // widening leaves an arbitrary break in the middle of a sentence.
        let mut g = Grid::new(4, 5);
        g.set_ascii_run(0, 0, b"abcde", DEFAULT_STYLE);
        g.set_wrapped(0, true);
        g.set_ascii_run(1, 0, b"fghij", DEFAULT_STYLE);
        g.set_wrapped(1, false);

        g.resize(4, 10);
        assert_eq!(text(&g, 0), "abcdefghij");
        assert!(!g.is_wrapped(0));
    }

    #[test]
    fn a_hard_newline_is_never_joined() {
        // Two separate lines that happen to be short must stay separate. This is
        // the whole reason the `wrapped` flag exists — without it, reflow would
        // glue unrelated lines together.
        let mut g = Grid::new(4, 5);
        g.set_ascii_run(0, 0, b"one", DEFAULT_STYLE);
        g.set_wrapped(0, false);
        g.set_ascii_run(1, 0, b"two", DEFAULT_STYLE);
        g.set_wrapped(1, false);

        g.resize(4, 20);
        assert_eq!(text(&g, 0).trim_end(), "one");
        assert_eq!(text(&g, 1).trim_end(), "two");
    }

    #[test]
    fn scrolling_carries_the_wrapped_flags_with_the_rows() {
        // If the flags do not move with their rows, reflow joins the wrong lines
        // after any scrolling — which is to say after a second of real output.
        let mut g = Grid::new(3, 4);
        g.set_wrapped(1, true);
        g.scroll_up(DEFAULT_STYLE);
        assert!(g.is_wrapped(0), "row 1 became row 0 and keeps its flag");
        assert!(
            !g.is_wrapped(2),
            "the freshly blanked row must not be wrapped"
        );
    }

    #[test]
    fn height_only_resize_does_not_rewrap() {
        let mut g = Grid::new(4, 10);
        g.set_ascii_run(3, 0, b"prompt", DEFAULT_STYLE);
        g.cursor = Cursor { row: 3, col: 6 };
        g.resize(8, 10);
        assert_eq!(g.rows(), 8);
        assert_eq!(g.cols(), 10);
        // Content is kept and the cursor still points at it.
        let found = (0..8).any(|r| text(&g, r).trim_end() == "prompt");
        assert!(found, "content lost on a height-only resize");
    }

    #[test]
    fn clear_row_respects_the_offset() {
        let mut g = Grid::new(3, 4);
        g.scroll_up(DEFAULT_STYLE);
        g.set_ascii_run(1, 0, b"xxxx", DEFAULT_STYLE);
        g.clear_row(1, DEFAULT_STYLE);
        assert_eq!(text(&g, 1), "    ");
    }
}
