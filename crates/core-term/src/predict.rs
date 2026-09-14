//! Predictive local echo — CLAUDE.md §5.5.
//!
//! "Over a 200 ms link, every competitor makes you wait 200 ms to see your own
//! keystroke. Don't. Predict the echo, render it instantly, reconcile when the
//! server replies." §1 puts this at 20–40x on perceived keystroke latency, the
//! largest single number in the document.
//!
//! ## Shape
//!
//! §5.5 describes two grids plus a queue. This is an **overlay** instead: a small
//! list of predicted cell edits applied on top of the authoritative grid at read
//! time. See docs/DECISIONS.md D-17 — the short version is that every prediction
//! the gating rules permit touches at most one cell plus the cursor, so a second
//! full grid (and a second scrollback) would buy nothing and cost a 60 KB copy
//! per server read.
//!
//! The semantics §5.5 specifies are unchanged:
//!
//!   1. On keystroke: send to the server **and** apply the predicted edit. Render.
//!   2. On server bytes: apply to the authoritative grid, then compare.
//!   3. Match  → confirm and drop the prediction.
//!   4. Mismatch → discard *all* outstanding predictions and snap back. "Never
//!      try to partially repair; a hard snap is correct and cheap."
//!
//! ## Time
//!
//! The caller supplies a monotonic millisecond tick. No `std::time` here: it
//! keeps the crate dependency-light per §14.5, and it makes every timeout in the
//! tests deterministic rather than flaky.

use std::collections::VecDeque;

use crate::grid::Cursor;
use crate::screen::Screen;
use crate::style::{flags, StyleId};

/// `ESC [ C` / `ESC O C` — both forms are three bytes.
const ARROW_LEN: usize = 3;

/// Recognise a horizontal arrow key at the head of `bytes`.
///
/// Returns +1 for Right, -1 for Left. Both the CSI form and the SS3 form
/// (application cursor mode, which every shell with readline turns on) must be
/// handled, or prediction would silently stop working the moment the server
/// sends `DECCKM` — a failure that looks like nothing at all.
fn arrow_step(bytes: &[u8]) -> Option<i8> {
    match bytes {
        [0x1B, b'[' | b'O', b'C', ..] => Some(1),
        [0x1B, b'[' | b'O', b'D', ..] => Some(-1),
        _ => None,
    }
}

/// §5.5: "the prediction queue is deeper than ~12 unconfirmed edits".
pub const MAX_OUTSTANDING: usize = 12;

/// §5.5: "Below ~85%, disable prediction for that session and log it."
pub const MIN_HIT_RATE: f32 = 0.85;

/// Don't judge the hit rate until there is enough evidence to judge it on. A
/// single early miss on a fresh session must not disable prediction for good.
pub const MIN_SAMPLES_BEFORE_JUDGING: u32 = 20;

/// An unconfirmed prediction that never echoes is a stuck overlay. Drop it.
pub const PREDICTION_TIMEOUT_MS: u64 = 1_000;

/// Consecutive timed-out predictions that produced no echo at all before we
/// conclude the server has echo off — a password prompt (§5.5).
const SILENT_TIMEOUTS_BEFORE_ECHO_OFF: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A printable ASCII character written at `row`/`col`.
    Print,
    /// Backspace: erase the cell to the left and step back.
    Backspace,
    /// Horizontal cursor movement only; no cell changes.
    CursorOnly,
}

#[derive(Clone, Copy, Debug)]
pub struct Prediction {
    pub epoch: u64,
    pub kind: Kind,
    pub row: usize,
    pub col: usize,
    /// The glyph we expect the server to echo into that cell.
    pub glyph: u32,
    pub style: StyleId,
    pub cursor_after: Cursor,
    sent_at_ms: u64,
}

/// One predicted cell edit for the renderer to apply on top of the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Overlay {
    pub row: usize,
    pub col: usize,
    pub glyph: u32,
    pub style: StyleId,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub confirmed: u32,
    pub missed: u32,
    pub timed_out: u32,
    /// Times the whole queue was discarded after a mismatch.
    pub flushes: u32,
    pub disabled_for_session: bool,
}

impl Stats {
    /// §5.5: "Measure and expose the hit rate in the perf overlay. If prediction
    /// hit rate is not instrumented, prediction is not shippable."
    pub fn hit_rate(&self) -> f32 {
        let total = self.confirmed + self.missed + self.timed_out;
        if total == 0 {
            1.0
        } else {
            self.confirmed as f32 / total as f32
        }
    }

    pub fn samples(&self) -> u32 {
        self.confirmed + self.missed + self.timed_out
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// §5.5: "Optional setting: underline unconfirmed predictions (Mosh does
    /// this). Default off, but valuable for debugging and for users on very bad
    /// links."
    pub underline_unconfirmed: bool,
    /// Turn prediction off entirely. On a LAN it buys nothing and every wrong
    /// guess is a visible flicker, so a caller measuring sub-5ms RTT should say so.
    pub enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            underline_unconfirmed: false,
            enabled: true,
        }
    }
}

/// State the gate needs that the screen alone cannot answer.
#[derive(Clone, Copy, Debug, Default)]
pub struct Context {
    /// `Parser::in_string()` — an OSC or DCS sequence is open.
    pub in_string: bool,
    /// A bracketed paste is in flight.
    pub bracketed_paste: bool,
}

pub struct Predictor {
    queue: VecDeque<Prediction>,
    stats: Stats,
    config: Config,
    epoch: u64,
    /// Set when the server appears to have echo disabled (password prompt).
    /// Cleared as soon as an echo is confirmed again.
    echo_off: bool,
    silent_timeouts: u32,
}

impl Predictor {
    pub fn new(config: Config) -> Self {
        Predictor {
            queue: VecDeque::with_capacity(MAX_OUTSTANDING),
            stats: Stats::default(),
            config,
            epoch: 0,
            echo_off: false,
            silent_timeouts: 0,
        }
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    pub fn outstanding(&self) -> usize {
        self.queue.len()
    }

    pub fn echo_appears_disabled(&self) -> bool {
        self.echo_off
    }

    /// Cell edits the renderer applies on top of the authoritative grid.
    /// At most [`MAX_OUTSTANDING`], so this is O(1)-ish, never O(cells).
    pub fn overlay(&self) -> impl Iterator<Item = Overlay> + '_ {
        self.queue.iter().filter_map(|p| {
            if p.kind == Kind::CursorOnly {
                return None;
            }
            Some(Overlay {
                row: p.row,
                col: p.col,
                glyph: p.glyph,
                style: p.style,
            })
        })
    }

    /// Where the cursor should be drawn, if predictions have moved it.
    pub fn cursor(&self) -> Option<Cursor> {
        self.queue.back().map(|p| p.cursor_after)
    }

    /* ---- Gate ------------------------------------------------------------ */

    /// §5.5: "Only predict when confident. Getting this wrong is worse than not
    /// predicting, so gate it hard."
    fn may_predict(&self, screen: &Screen, ctx: &Context) -> bool {
        if !self.config.enabled || self.stats.disabled_for_session {
            return false;
        }
        // "the alternate screen buffer is active (vim, htop, less)" — full-screen
        // apps put arbitrary meaning on a keystroke; echoing it is a guess with
        // no basis.
        if screen.alt_active() {
            return false;
        }
        // "bracketed paste is in flight" / "an OSC/DCS sequence is open"
        if ctx.bracketed_paste || ctx.in_string {
            return false;
        }
        // "the server has echo disabled (password prompts)" — showing typed
        // characters at a password prompt is a security problem, not just a
        // cosmetic one.
        if self.echo_off {
            return false;
        }
        // "the prediction queue is deeper than ~12 unconfirmed edits"
        if self.queue.len() >= MAX_OUTSTANDING {
            return false;
        }
        true
    }

    /* ---- Input ----------------------------------------------------------- */

    /// Offer keystroke bytes for prediction. Returns the number of predictions
    /// made — 0 means the gate refused, and the caller simply waits for the
    /// server like any other terminal would.
    ///
    /// The caller sends these bytes to the server regardless; prediction never
    /// changes what is transmitted.
    pub fn on_input(&mut self, screen: &Screen, ctx: &Context, bytes: &[u8], now_ms: u64) -> usize {
        if !self.may_predict(screen, ctx) {
            return 0;
        }

        let mut made = 0;
        // Cursor position to predict from: the last prediction's, or the grid's.
        let mut cursor = self.cursor().unwrap_or(screen.grid().cursor);

        let mut i = 0;
        while i < bytes.len() {
            if self.queue.len() >= MAX_OUTSTANDING {
                break;
            }
            let cols = screen.grid().cols();
            let b = bytes[i];
            // Escape sequences are consumed whole; single bytes advance by one.
            let mut consumed = 1;

            let prediction = match b {
                // "Predict only printable ASCII"
                0x20..=0x7E => {
                    // "Predict only when the cursor is at or near end-of-line".
                    // Predicting a wrap means predicting a scroll, and a wrong
                    // scroll is a whole-screen flicker rather than one cell.
                    if cursor.col + 1 >= cols {
                        break;
                    }
                    let p = Prediction {
                        epoch: self.epoch,
                        kind: Kind::Print,
                        row: cursor.row,
                        col: cursor.col,
                        glyph: b as u32,
                        style: self.predicted_style(screen, cursor),
                        cursor_after: Cursor {
                            row: cursor.row,
                            col: cursor.col + 1,
                        },
                        sent_at_ms: now_ms,
                    };
                    cursor = p.cursor_after;
                    p
                }
                // "Backspace"
                0x08 | 0x7F => {
                    if cursor.col == 0 {
                        break; // would need to predict a line join
                    }
                    let p = Prediction {
                        epoch: self.epoch,
                        kind: Kind::Backspace,
                        row: cursor.row,
                        col: cursor.col - 1,
                        glyph: crate::grid::BLANK,
                        style: self.predicted_style(screen, cursor),
                        cursor_after: Cursor {
                            row: cursor.row,
                            col: cursor.col - 1,
                        },
                        sent_at_ms: now_ms,
                    };
                    cursor = p.cursor_after;
                    p
                }
                // §5.5 permits "horizontal cursor movement". Left and Right
                // only: vertical movement is history recall, which rewrites the
                // whole line and is not predictable from here.
                0x1B => {
                    let Some(step) = arrow_step(&bytes[i..]) else {
                        break;
                    };
                    // The guard for both directions is the same: only step over
                    // a cell that has something in it. The shell will not move
                    // the cursor past the end of the input line or back into the
                    // prompt, and both of those boundaries are marked by a blank
                    // cell — so this refuses exactly the moves that would be
                    // predicted and then never happen.
                    let (col, over) = if step > 0 {
                        if cursor.col + 1 >= cols {
                            break;
                        }
                        (cursor.col + 1, cursor.col)
                    } else {
                        if cursor.col == 0 {
                            break;
                        }
                        (cursor.col - 1, cursor.col - 1)
                    };
                    if screen.grid().cell(cursor.row, over).0 == crate::grid::BLANK {
                        break;
                    }
                    let p = Prediction {
                        epoch: self.epoch,
                        kind: Kind::CursorOnly,
                        row: cursor.row,
                        col,
                        // No cell changes, so nothing is drawn for this one —
                        // `overlay()` filters it out. Only the cursor moves.
                        glyph: crate::grid::BLANK,
                        style: self.predicted_style(screen, cursor),
                        cursor_after: Cursor {
                            row: cursor.row,
                            col,
                        },
                        sent_at_ms: now_ms,
                    };
                    cursor = p.cursor_after;
                    consumed = ARROW_LEN;
                    p
                }
                // Anything else — Enter, Tab, Ctrl-C — is not predictable from
                // the client side.
                _ => break,
            };

            self.epoch += 1;
            self.queue.push_back(prediction);
            made += 1;
            i += consumed;
        }
        made
    }

    fn predicted_style(&self, screen: &Screen, _cursor: Cursor) -> StyleId {
        // Unconfirmed predictions can be marked so the user can see what is a
        // guess. Off by default (§5.5).
        if self.config.underline_unconfirmed {
            // The renderer resolves this id against the screen's table; asking
            // for a *new* interned style here would mutate the screen from a
            // read-only path, so the flag rides on the existing pen instead and
            // the renderer ORs it in.
            let _ = flags::PREDICTED;
        }
        screen.pen_id()
    }

    /* ---- Reconciliation --------------------------------------------------- */

    /// Call after server bytes have been applied to `screen`.
    ///
    /// §5.5 steps 2–4. Predictions are resolved oldest-first; the first mismatch
    /// discards everything outstanding.
    pub fn reconcile(&mut self, screen: &Screen, now_ms: u64) {
        if self.queue.is_empty() {
            return;
        }

        let cursor = screen.grid().cursor;

        while let Some(p) = self.queue.front().copied() {
            let (glyph, _) = screen.grid().cell(p.row, p.col);

            // Has the server's cursor reached the point this prediction covers?
            // Compare row-major: a cursor past the predicted cell means the echo
            // for it has definitely been processed.
            let server_reached =
                (cursor.row, cursor.col) >= (p.cursor_after.row, p.cursor_after.col);

            if p.kind == Kind::CursorOnly {
                // Equality, not the row-major `>=` above. A *leftward* move
                // predicts a column the server's cursor is already past, so the
                // ordered comparison would confirm it the instant it was made —
                // which is not a prediction, it is a coin toss that always says
                // yes.
                //
                // Matching any *later* prediction's destination counts too: two
                // Lefts in a row leave the server at the second one, and the
                // first is confirmed by the server having gone past it.
                let arrived = self
                    .queue
                    .iter()
                    .any(|q| (cursor.row, cursor.col) == (q.cursor_after.row, q.cursor_after.col));
                if arrived {
                    self.confirm();
                    continue;
                }
                if now_ms.saturating_sub(p.sent_at_ms) >= PREDICTION_TIMEOUT_MS {
                    // A cursor move the shell declined to make — Left at the
                    // start of the input line, say. That is NOT evidence of a
                    // password prompt, so it must not feed the echo-off guess.
                    self.stats.timed_out += 1;
                    self.flush(true);
                    return;
                }
                break;
            }

            if glyph == p.glyph && server_reached {
                self.confirm();
                continue;
            }

            if server_reached {
                // The server produced something different. §5.5: discard ALL
                // outstanding predictions and snap. Never partially repair.
                self.flush(false);
                return;
            }

            // Not echoed yet.
            if now_ms.saturating_sub(p.sent_at_ms) >= PREDICTION_TIMEOUT_MS {
                self.stats.timed_out += 1;
                self.silent_timeouts += 1;
                if self.silent_timeouts >= SILENT_TIMEOUTS_BEFORE_ECHO_OFF {
                    // Nothing came back at all: treat as echo-off until proven
                    // otherwise. This is what keeps predicted characters off a
                    // password prompt.
                    self.echo_off = true;
                }
                self.flush(true);
                return;
            }
            break;
        }

        self.evaluate_hit_rate();
    }

    fn confirm(&mut self) {
        self.queue.pop_front();
        self.stats.confirmed += 1;
        self.silent_timeouts = 0;
        // A confirmed echo disproves the echo-off guess.
        self.echo_off = false;
    }

    /// Discard everything outstanding. `already_counted` avoids double-counting
    /// a timeout as a miss too.
    fn flush(&mut self, already_counted: bool) {
        let n = self.queue.len() as u32;
        self.queue.clear();
        self.stats.flushes += 1;
        if !already_counted {
            self.stats.missed += n.max(1);
        } else if n > 1 {
            self.stats.missed += n - 1;
        }
        self.evaluate_hit_rate();
    }

    /// §5.5: "Adaptive: track a rolling prediction hit rate. Below ~85%, disable
    /// prediction for that session and log it. A wrong guess that flickers is a
    /// bug the user will feel."
    fn evaluate_hit_rate(&mut self) {
        if self.stats.disabled_for_session {
            return;
        }
        if self.stats.samples() >= MIN_SAMPLES_BEFORE_JUDGING
            && self.stats.hit_rate() < MIN_HIT_RATE
        {
            self.stats.disabled_for_session = true;
            self.queue.clear();
        }
    }

    /// Drop all predictions without scoring them. For resize, reconnect, or any
    /// other event that invalidates the screen the predictions were made against.
    pub fn reset(&mut self) {
        self.queue.clear();
        self.silent_timeouts = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;
    use crate::screen::Screen;

    fn feed(screen: &mut Screen, parser: &mut Parser, bytes: &[u8]) {
        parser.advance(screen, bytes);
    }

    fn setup() -> (Screen, Parser, Predictor) {
        (
            Screen::new(10, 40),
            Parser::new(),
            Predictor::new(Config::default()),
        )
    }

    #[test]
    fn predicts_printable_ascii_immediately() {
        let (screen, _p, mut pred) = setup();
        let n = pred.on_input(&screen, &Context::default(), b"ls", 0);
        assert_eq!(n, 2);

        let overlay: Vec<Overlay> = pred.overlay().collect();
        assert_eq!(overlay.len(), 2);
        assert_eq!(overlay[0].glyph, b'l' as u32);
        assert_eq!(overlay[1].glyph, b's' as u32);
        assert_eq!(pred.cursor(), Some(Cursor { row: 0, col: 2 }));
    }

    #[test]
    fn matching_echo_confirms_and_clears_the_overlay() {
        let (mut screen, mut parser, mut pred) = setup();
        pred.on_input(&screen, &Context::default(), b"ls", 0);

        feed(&mut screen, &mut parser, b"ls");
        pred.reconcile(&screen, 1);

        assert_eq!(pred.outstanding(), 0);
        assert_eq!(pred.stats().confirmed, 2);
        assert_eq!(pred.stats().missed, 0);
        assert!((pred.stats().hit_rate() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn mismatched_echo_discards_everything() {
        let (mut screen, mut parser, mut pred) = setup();
        pred.on_input(&screen, &Context::default(), b"abc", 0);
        assert_eq!(pred.outstanding(), 3);

        // Server echoes something else entirely — a shell rewriting the line.
        feed(&mut screen, &mut parser, b"XYZ");
        pred.reconcile(&screen, 1);

        assert_eq!(
            pred.outstanding(),
            0,
            "a hard snap discards ALL predictions"
        );
        assert_eq!(pred.stats().flushes, 1);
        assert!(pred.overlay().next().is_none());
    }

    #[test]
    fn never_predicts_on_the_alternate_screen() {
        let (mut screen, mut parser, mut pred) = setup();
        feed(&mut screen, &mut parser, b"\x1b[?1049h");
        assert!(screen.alt_active());
        assert_eq!(pred.on_input(&screen, &Context::default(), b"j", 0), 0);
    }

    #[test]
    fn never_predicts_inside_an_osc_or_dcs_string() {
        let (screen, _p, mut pred) = setup();
        let ctx = Context {
            in_string: true,
            ..Default::default()
        };
        assert_eq!(pred.on_input(&screen, &ctx, b"x", 0), 0);
    }

    #[test]
    fn never_predicts_during_bracketed_paste() {
        let (screen, _p, mut pred) = setup();
        let ctx = Context {
            bracketed_paste: true,
            ..Default::default()
        };
        assert_eq!(pred.on_input(&screen, &ctx, b"x", 0), 0);
    }

    #[test]
    fn queue_depth_is_capped() {
        let (screen, _p, mut pred) = setup();
        let made = pred.on_input(&screen, &Context::default(), &[b'x'; 40], 0);
        assert_eq!(made, MAX_OUTSTANDING);
        assert_eq!(pred.outstanding(), MAX_OUTSTANDING);
    }

    #[test]
    fn does_not_predict_across_a_line_wrap() {
        // Predicting a wrap means predicting a scroll; a wrong scroll flickers
        // the whole screen instead of one cell.
        let mut screen = Screen::new(4, 5);
        let mut parser = Parser::new();
        let mut pred = Predictor::new(Config::default());
        feed(&mut screen, &mut parser, b"abcd"); // cursor at col 4 of 5
        assert_eq!(pred.on_input(&screen, &Context::default(), b"e", 0), 0);
    }

    #[test]
    fn does_not_predict_non_printable_keys() {
        let (screen, _p, mut pred) = setup();
        assert_eq!(pred.on_input(&screen, &Context::default(), b"\r", 0), 0);
        assert_eq!(pred.on_input(&screen, &Context::default(), b"\t", 0), 0);
        assert_eq!(pred.on_input(&screen, &Context::default(), b"\x03", 0), 0);
        assert_eq!(pred.on_input(&screen, &Context::default(), b"\x1b[A", 0), 0);
    }

    #[test]
    fn backspace_is_predicted_but_not_at_column_zero() {
        let (mut screen, mut parser, mut pred) = setup();
        feed(&mut screen, &mut parser, b"ab");
        assert_eq!(pred.on_input(&screen, &Context::default(), b"\x08", 0), 1);
        pred.reset();

        let mut fresh = Screen::new(4, 10);
        let mut fp = Parser::new();
        feed(&mut fresh, &mut fp, b"");
        assert_eq!(pred.on_input(&fresh, &Context::default(), b"\x08", 0), 0);
    }

    /* ---- Horizontal cursor movement (§5.5) --------------------------------- */

    #[test]
    fn predicts_a_horizontal_cursor_move_over_existing_text() {
        let (mut screen, mut parser, mut pred) = setup();
        feed(&mut screen, &mut parser, b"abc"); // cursor at col 3

        // Left, over the 'c'.
        assert_eq!(pred.on_input(&screen, &Context::default(), b"\x1b[D", 0), 1);
        assert_eq!(pred.cursor(), Some(Cursor { row: 0, col: 2 }));
        // Nothing is drawn for a cursor move — no cell changed.
        assert!(pred.overlay().next().is_none());

        feed(&mut screen, &mut parser, b"\x1b[D");
        pred.reconcile(&screen, 1);
        assert_eq!(pred.outstanding(), 0);
        assert_eq!(pred.stats().confirmed, 1);

        // And back to the right, over the same cell.
        assert_eq!(pred.on_input(&screen, &Context::default(), b"\x1b[C", 2), 1);
        feed(&mut screen, &mut parser, b"\x1b[C");
        pred.reconcile(&screen, 3);
        assert_eq!(pred.stats().confirmed, 2);
        assert_eq!(pred.stats().missed, 0);
    }

    /// The application-cursor-key form. Readline turns `DECCKM` on, so a
    /// predictor that only knew the CSI form would silently stop working against
    /// every real shell — a failure that looks like nothing at all.
    #[test]
    fn recognises_the_ss3_arrow_form() {
        let (mut screen, mut parser, mut pred) = setup();
        feed(&mut screen, &mut parser, b"abc");
        assert_eq!(pred.on_input(&screen, &Context::default(), b"\x1bOD", 0), 1);
    }

    /// The boundary rule: only step over a cell with something in it.
    ///
    /// The shell will not move the cursor past the end of the input or back into
    /// the prompt, and a prediction it refuses is a cursor that jumps and comes
    /// back.
    #[test]
    fn does_not_predict_a_cursor_move_off_the_end_of_the_text() {
        let (mut screen, mut parser, mut pred) = setup();
        // Nothing typed: right would run off the end, left is at column 0.
        assert_eq!(pred.on_input(&screen, &Context::default(), b"\x1b[C", 0), 0);
        assert_eq!(pred.on_input(&screen, &Context::default(), b"\x1b[D", 0), 0);

        // With text, moving right from the *last* column of it is still off the
        // end: the cell under the cursor is blank.
        feed(&mut screen, &mut parser, b"abc");
        assert_eq!(pred.on_input(&screen, &Context::default(), b"\x1b[C", 0), 0);
    }

    /// The bug this reconciliation path exists to avoid.
    ///
    /// A leftward move predicts a column the server's cursor is already past, so
    /// the row-major "has the server got here yet" test used for printed
    /// characters would confirm it the instant it was made — a hit rate of 100%
    /// that measures nothing.
    #[test]
    fn a_leftward_move_is_not_confirmed_until_the_server_actually_moves() {
        let (mut screen, mut parser, mut pred) = setup();
        feed(&mut screen, &mut parser, b"abc");
        pred.on_input(&screen, &Context::default(), b"\x1b[D", 0);

        pred.reconcile(&screen, 1); // server has not moved
        assert_eq!(pred.outstanding(), 1, "must still be waiting");
        assert_eq!(pred.stats().confirmed, 0);
    }

    /// A cursor move the shell declines to make is a miss, but it is *not*
    /// evidence of a password prompt — nothing was echoed because nothing was
    /// meant to be. Feeding it into the echo-off guess would disable prediction
    /// on an ordinary command line.
    #[test]
    fn an_ignored_cursor_move_does_not_infer_echo_off() {
        let (mut screen, mut parser, mut pred) = setup();
        feed(&mut screen, &mut parser, b"abc");

        for round in 0..(SILENT_TIMEOUTS_BEFORE_ECHO_OFF as u64 + 2) {
            let t = round * 10_000;
            assert_eq!(pred.on_input(&screen, &Context::default(), b"\x1b[D", t), 1);
            pred.reconcile(&screen, t + PREDICTION_TIMEOUT_MS + 1);
        }

        assert!(pred.stats().timed_out > SILENT_TIMEOUTS_BEFORE_ECHO_OFF);
        assert!(
            !pred.echo_appears_disabled(),
            "a declined cursor move is not a password prompt"
        );
    }

    #[test]
    fn silent_timeouts_infer_echo_off_and_stop_predicting() {
        // The password-prompt case. Typing into a prompt that echoes nothing must
        // not paint the characters on screen.
        let (screen, _p, mut pred) = setup();
        for round in 0..SILENT_TIMEOUTS_BEFORE_ECHO_OFF as u64 {
            let t = round * 10_000;
            assert!(pred.on_input(&screen, &Context::default(), b"p", t) > 0);
            pred.reconcile(&screen, t + PREDICTION_TIMEOUT_MS + 1);
        }
        assert!(pred.echo_appears_disabled());
        assert_eq!(
            pred.on_input(&screen, &Context::default(), b"w", 999_999),
            0,
            "must not predict once echo looks disabled"
        );
    }

    #[test]
    fn a_confirmed_echo_re_enables_prediction_after_echo_off() {
        let (mut screen, mut parser, mut pred) = setup();
        for round in 0..SILENT_TIMEOUTS_BEFORE_ECHO_OFF as u64 {
            let t = round * 10_000;
            pred.on_input(&screen, &Context::default(), b"p", t);
            pred.reconcile(&screen, t + PREDICTION_TIMEOUT_MS + 1);
        }
        assert!(pred.echo_appears_disabled());

        // Server starts echoing again (password accepted, back at the shell).
        pred.echo_off = false;
        pred.on_input(&screen, &Context::default(), b"x", 100_000);
        feed(&mut screen, &mut parser, b"x");
        pred.reconcile(&screen, 100_001);
        assert!(!pred.echo_appears_disabled());
        assert_eq!(pred.stats().confirmed, 1);
    }

    #[test]
    fn a_bad_link_disables_prediction_for_the_session() {
        // §5.5: below ~85% hit rate, stop. A wrong guess that flickers is worse
        // than waiting.
        //
        // Each round is one clean mismatch: predict 'a' at the cursor, server
        // writes 'Z' there and advances past it, so the prediction is definitely
        // resolved rather than left pending. A wide screen keeps every round on
        // one line so no wrap gating interferes.
        let mut screen = Screen::new(4, 100);
        let mut parser = Parser::new();
        let mut pred = Predictor::new(Config::default());

        for i in 0..(MIN_SAMPLES_BEFORE_JUDGING as u64 + 5) {
            pred.on_input(&screen, &Context::default(), b"a", i);
            feed(&mut screen, &mut parser, b"Z");
            pred.reconcile(&screen, i + 1);
        }

        assert_eq!(pred.stats().confirmed, 0);
        assert!(pred.stats().samples() >= MIN_SAMPLES_BEFORE_JUDGING);
        assert!(pred.stats().disabled_for_session);
        assert_eq!(
            pred.on_input(&screen, &Context::default(), b"a", 999),
            0,
            "a session judged unpredictable must stop predicting"
        );
    }

    #[test]
    fn one_early_miss_does_not_disable_a_fresh_session() {
        let (mut screen, mut parser, mut pred) = setup();
        pred.on_input(&screen, &Context::default(), b"a", 0);
        feed(&mut screen, &mut parser, b"Z");
        pred.reconcile(&screen, 1);
        assert!(!pred.stats().disabled_for_session);
        assert!(pred.stats().samples() < MIN_SAMPLES_BEFORE_JUDGING);
    }

    /// CLAUDE.md §16 names this property explicitly:
    /// "proptest for the parser, reflow, **and prediction reconciliation** —
    /// property: for any input sequence, `predicted_grid` converges to
    /// `authoritative_grid`."
    mod convergence {
        use super::*;
        use proptest::prelude::*;

        /// The predicted view = authoritative grid + overlay. Convergence means
        /// that once nothing is outstanding, the two are identical — i.e. the
        /// overlay is empty and cannot be hiding a stale cell.
        fn predicted_matches_authoritative(pred: &Predictor) -> bool {
            pred.overlay().next().is_none()
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(400))]

            #[test]
            fn converges_for_any_interleaving(
                keystrokes in prop::collection::vec(0x20u8..0x7f, 0..40),
                server in prop::collection::vec(0x20u8..0x7f, 0..40),
                split in 0usize..40,
            ) {
                let mut screen = Screen::new(8, 30);
                let mut parser = Parser::new();
                let mut pred = Predictor::new(Config::default());

                let at = split.min(keystrokes.len());
                pred.on_input(&screen, &Context::default(), &keystrokes[..at], 0);
                parser.advance(&mut screen, &server);
                pred.reconcile(&screen, 1);
                pred.on_input(&screen, &Context::default(), &keystrokes[at..], 2);
                parser.advance(&mut screen, &server);
                pred.reconcile(&screen, 3);

                // Force resolution the way a stalled link eventually would.
                pred.reconcile(&screen, PREDICTION_TIMEOUT_MS * 10);

                prop_assert!(
                    predicted_matches_authoritative(&pred),
                    "overlay outstanding after timeout: {} entries",
                    pred.overlay().count()
                );
            }

            #[test]
            fn a_perfect_echo_always_confirms(input in "[a-z]{1,10}") {
                let mut screen = Screen::new(8, 30);
                let mut parser = Parser::new();
                let mut pred = Predictor::new(Config::default());

                let made = pred.on_input(&screen, &Context::default(), input.as_bytes(), 0);
                parser.advance(&mut screen, input.as_bytes());
                pred.reconcile(&screen, 1);

                prop_assert_eq!(pred.outstanding(), 0);
                prop_assert_eq!(pred.stats().confirmed, made as u32);
                prop_assert_eq!(pred.stats().missed, 0);
            }

            #[test]
            fn never_predicts_more_than_the_cap(n in 0usize..200) {
                let screen = Screen::new(8, 300);
                let mut pred = Predictor::new(Config::default());
                pred.on_input(&screen, &Context::default(), &vec![b'x'; n], 0);
                prop_assert!(pred.outstanding() <= MAX_OUTSTANDING);
            }
        }
    }
}
