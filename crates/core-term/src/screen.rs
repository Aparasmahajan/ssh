//! The screen — a [`Perform`] that drives a [`Grid`] and feeds [`Scrollback`].
//!
//! This is where parser actions become cell writes. It implements enough of
//! ECMA-48 to run a shell: cursor motion, erase, SGR, line feed with scroll, and
//! the alternate screen buffer.
//!
//! The alternate buffer matters beyond correctness: CLAUDE.md §5.5 makes it a
//! hard gate on predictive echo ("Never predict when the alternate screen buffer
//! is active"), so [`Screen::alt_active`] is part of the prediction contract, not
//! a detail.

use crate::grid::{Cursor, Grid, BLANK, WIDE_SPACER};
use crate::params::Params;
use crate::parser::Perform;
use crate::scrollback::{Row, Scrollback};
use crate::style::{color, flags, Style, StyleId, StyleTable, DEFAULT_STYLE};
use crate::width::char_width;

pub struct Screen {
    grid: Grid,
    alt: Option<Grid>,
    styles: StyleTable,
    scrollback: Scrollback,
    pen: Style,
    pen_id: StyleId,
    /// Deferred wrap (DEC "last column" behaviour): printing in the final column
    /// leaves the cursor there and sets this, so `CR` or a cursor move cancels
    /// the wrap instead of eating a line. Without it, an 80-char line in an
    /// 80-column terminal produces a spurious blank row.
    pending_wrap: bool,
    saved_cursor: Cursor,
    pub title: String,
}

impl Screen {
    pub fn new(rows: usize, cols: usize) -> Self {
        Screen {
            grid: Grid::new(rows, cols),
            alt: None,
            styles: StyleTable::new(),
            scrollback: Scrollback::with_default_capacity(),
            pen: Style::default(),
            pen_id: DEFAULT_STYLE,
            pending_wrap: false,
            saved_cursor: Cursor::default(),
            title: String::new(),
        }
    }

    #[inline]
    pub fn grid(&self) -> &Grid {
        &self.grid
    }

    #[inline]
    pub fn grid_mut(&mut self) -> &mut Grid {
        &mut self.grid
    }

    #[inline]
    pub fn styles(&self) -> &StyleTable {
        &self.styles
    }

    /// The current pen's interned id. Predictions adopt it so a predicted
    /// character is drawn in the same colour the real echo will arrive in —
    /// otherwise confirming a prediction would change the cell's appearance and
    /// produce a visible flicker on a *correct* guess.
    #[inline]
    pub fn pen_id(&self) -> StyleId {
        self.pen_id
    }

    #[inline]
    pub fn scrollback(&self) -> &Scrollback {
        &self.scrollback
    }

    /// §5.5 prediction gate.
    #[inline]
    pub fn alt_active(&self) -> bool {
        self.alt.is_some()
    }

    pub fn resize(&mut self, rows: usize, cols: usize) {
        self.grid.resize(rows, cols);
        if let Some(alt) = &mut self.alt {
            alt.resize(rows, cols);
        }
        // §5.6 splits reflow in two: the viewport is re-wrapped now (above,
        // bounded by rows x cols), history is queued and done in chunks. A drag
        // resize emits an event per pointer move, so the expensive half must not
        // run synchronously here.
        self.scrollback.request_reflow(cols);
        self.pending_wrap = false;
    }

    /// Do one chunk of pending scrollback reflow; `true` while more remains.
    ///
    /// Drive this from a scroll handler or an idle tick. Until it finishes, older
    /// history is still wrapped to the previous width — which is what "lazily" in
    /// §5.6 means, and is only visible if you scroll that far back within the few
    /// milliseconds it takes.
    pub fn reflow_step(&mut self) -> bool {
        self.scrollback.reflow_step()
    }

    pub fn reflow_pending(&self) -> bool {
        self.scrollback.reflow_pending()
    }

    /* ---- internals ------------------------------------------------------- */

    fn newline(&mut self) {
        let last = self.grid.rows() - 1;
        if self.grid.cursor.row < last {
            self.grid.cursor.row += 1;
            return;
        }

        // Capture the outgoing line BEFORE scrolling — `scroll_up` is O(1) and
        // simply stops addressing that row.
        //
        // The alternate screen is deliberately not captured: full-screen
        // applications redraw constantly, and storing their frames would fill
        // the ring with vim repaints and evict real history.
        if self.alt.is_none() {
            // The wrapped flag travels with the row. Without it, scrollback
            // reflow would join lines that merely sit next to each other
            // (§5.6, and `Scrollback::reflow_step`).
            let wrapped = self.grid.is_wrapped(0);
            let row = {
                let (glyph, style) = self.grid.row(0);
                Row::trimmed(glyph, style, wrapped)
            };
            self.scrollback.push(row);
        }
        self.grid.scroll_up(self.pen_id);
    }

    #[inline]
    fn advance_cursor(&mut self) {
        if self.grid.cursor.col + 1 < self.grid.cols() {
            self.grid.cursor.col += 1;
        } else {
            self.pending_wrap = true;
        }
    }

    fn flush_wrap(&mut self) {
        if self.pending_wrap {
            self.pending_wrap = false;
            // This row ran out of width and is continuing — NOT a newline. The
            // flag is what lets resize re-wrap it instead of joining unrelated
            // lines together (grid::resize).
            let row = self.grid.cursor.row;
            self.grid.set_wrapped(row, true);
            self.grid.cursor.col = 0;
            self.newline();
        }
    }

    fn set_pen(&mut self, style: Style) {
        self.pen = style;
        self.pen_id = self.styles.intern(style);
    }

    fn move_to(&mut self, row: usize, col: usize) {
        self.grid.cursor.row = row.min(self.grid.rows().saturating_sub(1));
        self.grid.cursor.col = col.min(self.grid.cols().saturating_sub(1));
        self.pending_wrap = false;
    }

    fn enter_alt(&mut self) {
        if self.alt.is_none() {
            let fresh = Grid::new(self.grid.rows(), self.grid.cols());
            let main = std::mem::replace(&mut self.grid, fresh);
            self.alt = Some(main);
        }
    }

    fn leave_alt(&mut self) {
        if let Some(main) = self.alt.take() {
            self.grid = main;
            self.grid.mark_all_dirty();
        }
    }

    /// SGR — Select Graphic Rendition.
    fn sgr(&mut self, params: &Params) {
        if params.is_empty() {
            self.set_pen(Style::default());
            return;
        }
        let mut pen = self.pen;
        let mut i = 0;
        while i < params.len() {
            match params.raw(i) {
                0 => pen = Style::default(),
                1 => pen.flags |= flags::BOLD,
                2 => pen.flags |= flags::DIM,
                3 => pen.flags |= flags::ITALIC,
                4 => pen.flags |= flags::UNDERLINE,
                5 => pen.flags |= flags::BLINK,
                7 => pen.flags |= flags::REVERSE,
                8 => pen.flags |= flags::HIDDEN,
                9 => pen.flags |= flags::STRIKE,
                22 => pen.flags &= !(flags::BOLD | flags::DIM),
                23 => pen.flags &= !flags::ITALIC,
                24 => pen.flags &= !flags::UNDERLINE,
                25 => pen.flags &= !flags::BLINK,
                27 => pen.flags &= !flags::REVERSE,
                28 => pen.flags &= !flags::HIDDEN,
                29 => pen.flags &= !flags::STRIKE,
                n @ 30..=37 => pen.fg = color::indexed((n - 30) as u8),
                38 => {
                    if let Some((c, used)) = extended_color(params, i) {
                        pen.fg = c;
                        i += used;
                    }
                }
                39 => pen.fg = color::DEFAULT_FG,
                n @ 40..=47 => pen.bg = color::indexed((n - 40) as u8),
                48 => {
                    if let Some((c, used)) = extended_color(params, i) {
                        pen.bg = c;
                        i += used;
                    }
                }
                49 => pen.bg = color::DEFAULT_BG,
                n @ 90..=97 => pen.fg = color::indexed((n - 90 + 8) as u8),
                n @ 100..=107 => pen.bg = color::indexed((n - 100 + 8) as u8),
                _ => {}
            }
            i += 1;
        }
        self.set_pen(pen);
    }
}

/// `38;5;N` (indexed) and `38;2;R;G;B` (truecolour). Returns the colour and how
/// many extra parameters were consumed.
fn extended_color(params: &Params, i: usize) -> Option<(u32, usize)> {
    match params.raw(i + 1) {
        5 => Some((color::indexed(params.raw(i + 2) as u8), 2)),
        2 => Some((
            color::rgb(
                params.raw(i + 2) as u8,
                params.raw(i + 3) as u8,
                params.raw(i + 4) as u8,
            ),
            4,
        )),
        _ => None,
    }
}

impl Perform for Screen {
    fn print(&mut self, c: char) {
        let w = char_width(c);

        // §5.1 zero-width: combining marks and variation selectors belong to the
        // cell before them. Advancing for one would push the rest of the line
        // right by a column per accent.
        if w == 0 {
            return;
        }

        self.flush_wrap();

        if w == 2 {
            // A double-width character cannot straddle the right edge, so wrap
            // BEFORE writing rather than clipping it in half.
            if self.grid.cursor.col + 1 >= self.grid.cols() {
                self.grid.cursor.col = 0;
                self.newline();
            }
            let (row, col) = (self.grid.cursor.row, self.grid.cursor.col);
            self.grid.set(row, col, c as u32, self.pen_id);
            // §5.1: "the second carries WIDE_SPACER". The renderer skips it, and
            // the cursor must step over it so a following character does not land
            // inside the glyph.
            self.grid.set(row, col + 1, WIDE_SPACER, self.pen_id);
            if col + 2 < self.grid.cols() {
                self.grid.cursor.col = col + 2;
            } else {
                self.grid.cursor.col = self.grid.cols() - 1;
                self.pending_wrap = true;
            }
            return;
        }

        let (row, col) = (self.grid.cursor.row, self.grid.cursor.col);
        self.grid.set(row, col, c as u32, self.pen_id);
        self.advance_cursor();
    }

    /// The §5.2 bulk path's destination: one damage mark and two contiguous
    /// writes per run, instead of per character.
    fn print_ascii(&mut self, bytes: &[u8]) {
        let mut rest = bytes;
        while !rest.is_empty() {
            self.flush_wrap();
            let (row, col) = (self.grid.cursor.row, self.grid.cursor.col);
            let written = self.grid.set_ascii_run(row, col, rest, self.pen_id);
            if written == 0 {
                break;
            }
            rest = &rest[written..];
            if col + written >= self.grid.cols() {
                self.grid.cursor.col = self.grid.cols() - 1;
                self.pending_wrap = true;
            } else {
                self.grid.cursor.col = col + written;
            }
        }
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' | 0x0B | 0x0C => {
                self.pending_wrap = false;
                self.newline();
            }
            b'\r' => {
                self.pending_wrap = false;
                self.grid.cursor.col = 0;
            }
            0x08 => {
                // Backspace. Stepping back onto a WIDE_SPACER would leave the
                // cursor inside a double-width glyph, so step over the pair.
                self.pending_wrap = false;
                let col = self.grid.cursor.col.saturating_sub(1);
                let on_spacer =
                    col > 0 && self.grid.cell(self.grid.cursor.row, col).0 == WIDE_SPACER;
                self.grid.cursor.col = if on_spacer { col - 1 } else { col };
            }
            b'\t' => {
                self.pending_wrap = false;
                let next = ((self.grid.cursor.col / 8) + 1) * 8;
                self.grid.cursor.col = next.min(self.grid.cols() - 1);
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: u8) {
        if ignore {
            return;
        }
        let private = intermediates.first() == Some(&b'?');
        let cursor = self.grid.cursor;

        match action {
            b'A' => self.move_to(
                cursor.row.saturating_sub(params.get_or(0, 1) as usize),
                cursor.col,
            ),
            b'B' => self.move_to(cursor.row + params.get_or(0, 1) as usize, cursor.col),
            b'C' => self.move_to(cursor.row, cursor.col + params.get_or(0, 1) as usize),
            b'D' => self.move_to(
                cursor.row,
                cursor.col.saturating_sub(params.get_or(0, 1) as usize),
            ),
            b'G' => self.move_to(cursor.row, params.get_or(0, 1) as usize - 1),
            b'H' | b'f' => self.move_to(
                params.get_or(0, 1) as usize - 1,
                params.get_or(1, 1) as usize - 1,
            ),
            b'J' => {
                let pen = self.pen_id;
                match params.raw(0) {
                    0 => {
                        self.grid.clear_row_from(cursor.row, cursor.col, pen);
                        for r in cursor.row + 1..self.grid.rows() {
                            self.grid.clear_row(r, pen);
                        }
                    }
                    1 => {
                        for r in 0..cursor.row {
                            self.grid.clear_row(r, pen);
                        }
                        for c in 0..=cursor.col {
                            self.grid.set(cursor.row, c, BLANK, pen);
                        }
                    }
                    _ => self.grid.clear_all(pen),
                }
            }
            b'K' => {
                let pen = self.pen_id;
                match params.raw(0) {
                    0 => self.grid.clear_row_from(cursor.row, cursor.col, pen),
                    1 => {
                        for c in 0..=cursor.col {
                            self.grid.set(cursor.row, c, BLANK, pen);
                        }
                    }
                    _ => self.grid.clear_row(cursor.row, pen),
                }
            }
            b'm' => self.sgr(params),
            b'h' if private => {
                // 1049 / 1047 / 47: alternate screen
                if matches!(params.raw(0), 1049 | 1047 | 47) {
                    self.saved_cursor = cursor;
                    self.enter_alt();
                    self.move_to(0, 0);
                }
            }
            b'l' if private => {
                if matches!(params.raw(0), 1049 | 1047 | 47) {
                    self.leave_alt();
                    let c = self.saved_cursor;
                    self.move_to(c.row, c.col);
                }
            }
            b's' => self.saved_cursor = cursor,
            b'u' => {
                let c = self.saved_cursor;
                self.move_to(c.row, c.col);
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
        match byte {
            b'7' => self.saved_cursor = self.grid.cursor,
            b'8' => {
                let c = self.saved_cursor;
                self.move_to(c.row, c.col);
            }
            b'M' => {
                // Reverse index
                if self.grid.cursor.row == 0 {
                    self.grid.mark_all_dirty();
                } else {
                    self.grid.cursor.row -= 1;
                }
            }
            b'c' => {
                self.grid.clear_all(DEFAULT_STYLE);
                self.set_pen(Style::default());
                self.move_to(0, 0);
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        // OSC 0 and 2 set the window title.
        if let (Some(&kind), Some(&value)) = (params.first(), params.get(1)) {
            if kind == b"0" || kind == b"2" {
                if let Ok(s) = std::str::from_utf8(value) {
                    // A title arrives from the remote host. It is display data,
                    // never a command, and it is bounded so a hostile host cannot
                    // grow it without limit.
                    self.title = s.chars().take(512).collect();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    fn feed(screen: &mut Screen, input: &[u8]) {
        let mut p = Parser::new();
        p.advance(screen, input);
    }

    fn line(s: &Screen, row: usize) -> String {
        (0..s.grid().cols())
            .map(|c| char::from_u32(s.grid().cell(row, c).0).unwrap_or(' '))
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn prints_and_wraps_at_the_right_edge() {
        let mut s = Screen::new(3, 5);
        feed(&mut s, b"abcdefg");
        assert_eq!(line(&s, 0), "abcde");
        assert_eq!(line(&s, 1), "fg");
    }

    #[test]
    fn exact_width_line_does_not_emit_a_blank_row() {
        // The deferred-wrap rule. Without it, "abcde\r\n" in a 5-col terminal
        // leaves an empty line behind.
        let mut s = Screen::new(3, 5);
        feed(&mut s, b"abcde\r\nxy");
        assert_eq!(line(&s, 0), "abcde");
        assert_eq!(line(&s, 1), "xy");
    }

    /// Scrollback reflow, end to end through the real parser (§5.6).
    ///
    /// The part only this test covers is the handoff: a row that auto-wrapped in
    /// the grid must arrive in scrollback still marked as a continuation. If the
    /// flag is dropped at that boundary — which is easy, since `Row::trimmed`
    /// takes the cell planes and nothing else — widening rejoins nothing and the
    /// history stays broken at the old width with no error anywhere.
    #[test]
    fn history_rejoins_a_wrapped_line_after_a_widening_resize() {
        let mut s = Screen::new(2, 6);
        // "hello world" wraps at 6 columns, then two newlines push it into
        // history.
        feed(&mut s, b"hello world\r\nsecond\r\nthird\r\n");

        let before: Vec<String> = s.scrollback().iter().map(row_text).collect();
        assert!(
            before.iter().any(|l| l == "hello "),
            "expected the wrapped halves in history, got {before:?}"
        );

        s.resize(2, 40);
        assert!(s.reflow_pending(), "history reflow is queued, not immediate");
        while s.reflow_step() {}

        let after: Vec<String> = s.scrollback().iter().map(row_text).collect();
        assert!(
            after.iter().any(|l| l == "hello world"),
            "the wrapped line was not rejoined: {after:?}"
        );
        assert!(
            after.iter().any(|l| l == "second"),
            "an unrelated line was lost or joined: {after:?}"
        );
    }

    fn row_text(r: &crate::scrollback::Row) -> String {
        r.glyph
            .iter()
            .map(|&g| char::from_u32(g).unwrap_or(' '))
            .collect()
    }

    #[test]
    fn scrolling_feeds_scrollback() {
        let mut s = Screen::new(2, 8);
        feed(&mut s, b"one\r\ntwo\r\nthree");
        assert_eq!(s.scrollback().len(), 1);
        assert_eq!(
            s.scrollback()
                .get(0)
                .unwrap()
                .glyph
                .iter()
                .map(|&g| char::from_u32(g).unwrap())
                .collect::<String>(),
            "one"
        );
        assert_eq!(line(&s, 1), "three");
    }

    #[test]
    fn cursor_addressing_is_one_based() {
        let mut s = Screen::new(4, 10);
        feed(&mut s, b"\x1b[2;3HX");
        assert_eq!(s.grid().cell(1, 2).0, b'X' as u32);
    }

    #[test]
    fn erase_in_line_from_cursor() {
        let mut s = Screen::new(2, 10);
        feed(&mut s, b"abcdefgh\x1b[1;4H\x1b[K");
        assert_eq!(line(&s, 0), "abc");
    }

    #[test]
    fn sgr_sets_and_resets_the_pen() {
        let mut s = Screen::new(2, 10);
        feed(&mut s, b"\x1b[1;31mA\x1b[0mB");
        let (_, style_a) = s.grid().cell(0, 0);
        let (_, style_b) = s.grid().cell(0, 1);
        assert_ne!(style_a, style_b);
        assert!(s.styles().get(style_a).flags & flags::BOLD != 0);
        assert_eq!(s.styles().get(style_b), Style::default());
    }

    #[test]
    fn truecolour_sgr_with_semicolons_and_colons_agree() {
        let mut a = Screen::new(1, 4);
        let mut b = Screen::new(1, 4);
        feed(&mut a, b"\x1b[38;2;255;100;0mX");
        feed(&mut b, b"\x1b[38:2:255:100:0mX");
        let sa = a.styles().get(a.grid().cell(0, 0).1);
        let sb = b.styles().get(b.grid().cell(0, 0).1);
        assert_eq!(sa.fg, color::rgb(255, 100, 0));
        assert_eq!(sa, sb);
    }

    #[test]
    fn alternate_screen_round_trips_and_is_not_scrollback() {
        let mut s = Screen::new(2, 8);
        feed(&mut s, b"main\r\n");
        assert!(!s.alt_active());

        feed(&mut s, b"\x1b[?1049h");
        assert!(s.alt_active());
        feed(&mut s, b"alt1\r\nalt2\r\nalt3");
        let sb_len = s.scrollback().len();

        feed(&mut s, b"\x1b[?1049l");
        assert!(!s.alt_active());
        assert_eq!(line(&s, 0), "main");
        assert_eq!(
            s.scrollback().len(),
            sb_len,
            "vim repaints must not fill the scrollback ring"
        );
    }

    #[test]
    fn osc_sets_the_title() {
        let mut s = Screen::new(2, 8);
        feed(&mut s, b"\x1b]0;deploy@linux-01\x07");
        assert_eq!(s.title, "deploy@linux-01");
    }

    #[test]
    fn backspace_and_tab() {
        let mut s = Screen::new(1, 20);
        feed(&mut s, b"abc\x08X");
        assert_eq!(line(&s, 0), "abX");
        let mut s2 = Screen::new(1, 20);
        feed(&mut s2, b"a\tb");
        assert_eq!(s2.grid().cell(0, 8).0, b'b' as u32);
    }

    #[test]
    fn an_auto_wrapped_line_is_marked_and_a_newline_is_not() {
        // The distinction reflow depends on: text that ran off the edge should
        // rejoin when the window widens; text after Enter never should.
        let mut s = Screen::new(4, 5);
        feed(&mut s, b"abcdefg");
        assert!(s.grid().is_wrapped(0), "auto-wrap must set the flag");

        let mut s2 = Screen::new(4, 5);
        feed(
            &mut s2, b"abc
def",
        );
        assert!(!s2.grid().is_wrapped(0), "a newline is not a wrap");
    }

    #[test]
    fn resizing_narrower_keeps_the_text() {
        // End to end through the screen: type a line wider than the new width
        // and confirm nothing is lost.
        let mut s = Screen::new(4, 12);
        feed(&mut s, b"abcdefghijkl");
        s.resize(4, 6);
        let joined: String = (0..4).map(|r| line(&s, r)).collect::<Vec<_>>().join("");
        assert!(
            joined.contains("abcdef") && joined.contains("ghijkl"),
            "text lost on narrowing: {joined:?}"
        );
    }

    #[test]
    fn a_wide_character_occupies_two_cells() {
        let mut s = Screen::new(2, 10);
        feed(&mut s, "a日b".as_bytes());
        assert_eq!(s.grid().cell(0, 0).0, b'a' as u32);
        assert_eq!(s.grid().cell(0, 1).0, '日' as u32);
        assert_eq!(
            s.grid().cell(0, 2).0,
            crate::grid::WIDE_SPACER,
            "the second cell must be the spacer, not a copy or a blank"
        );
        // 'b' lands at column 3, not 2 — this is the alignment the whole width
        // module exists for.
        assert_eq!(s.grid().cell(0, 3).0, b'b' as u32);
    }

    #[test]
    fn a_wide_character_wraps_instead_of_straddling_the_edge() {
        // Five columns, four taken: a double-width glyph cannot be split across
        // the boundary, so it must move to the next line whole.
        let mut s = Screen::new(3, 5);
        feed(&mut s, "abcd日".as_bytes());
        assert_eq!(line(&s, 0), "abcd");
        assert_eq!(s.grid().cell(1, 0).0, '日' as u32);
        assert_eq!(s.grid().cell(1, 1).0, crate::grid::WIDE_SPACER);
    }

    #[test]
    fn combining_marks_do_not_advance_the_cursor() {
        // "e" + combining acute occupies one cell, not two, or every accented
        // word pushes the rest of the line right.
        let mut s = Screen::new(1, 10);
        feed(&mut s, "e\u{0301}x".as_bytes());
        assert_eq!(s.grid().cell(0, 0).0, b'e' as u32);
        assert_eq!(s.grid().cell(0, 1).0, b'x' as u32);
    }

    #[test]
    fn backspace_steps_over_a_wide_character() {
        // Landing on the spacer would put the cursor inside a glyph, and the
        // next character would overwrite half of it.
        let mut s = Screen::new(1, 10);
        feed(&mut s, "日".as_bytes());
        assert_eq!(s.grid().cursor.col, 2);
        feed(&mut s, b"\x08");
        assert_eq!(s.grid().cursor.col, 0, "must skip past the WIDE_SPACER");
    }

    #[test]
    fn damage_is_confined_to_touched_rows() {
        let mut s = Screen::new(10, 20);
        s.grid_mut().clear_damage();
        feed(&mut s, b"\x1b[5;1Hhello");
        let dirty: Vec<usize> = s.grid().dirty_rows().collect();
        assert_eq!(dirty, vec![4]);
    }
}
