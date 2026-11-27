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
    /// The epoch it was made in. Only a confirmation from the same epoch can
    /// make it visible; see [`Predictor`].
    pub epoch: u64,
    /// Drawn. False while its epoch is unconfirmed.
    pub shown: bool,
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

/// Hit-rate counters cover **drawn** predictions only. A tentative one that
/// turns out wrong was never on screen, so it cost the user nothing; counting
/// it would let every password prompt (all tentative, all "wrong") push the
/// session towards the §5.5 shut-off.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub confirmed: u32,
    pub missed: u32,
    pub timed_out: u32,
    /// Times the whole queue was discarded after a mismatch.
    pub flushes: u32,
    /// Tentative predictions resolved without ever being drawn, either way.
    pub withheld: u32,
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

/// Where the server's cursor rests between keystrokes, and what is beside it.
#[derive(Clone, Copy, Debug)]
struct Rest {
    cursor: Cursor,
    /// The glyph left of the cursor, if it is not in column 0.
    left: Option<u32>,
    /// Set by a confirmed echo (we were typing on this line), not observed
    /// after output. Only then can a cursor seen elsewhere on the same row be
    /// the shell's redraw in progress; after Enter, a first read of output is
    /// no place to predict from.
    from_echo: bool,
}

impl Rest {
    fn at(screen: &Screen, cursor: Cursor) -> Rest {
        let left = (cursor.col > 0).then(|| screen.grid().cell(cursor.row, cursor.col - 1).0);
        Rest { cursor, left, from_echo: false }
    }

    /// The screen still shows the shell waiting here. Both checks matter: a
    /// prompt written over the line can leave the cursor in the very column
    /// the echo did, but not with the echoed character beside it.
    fn holds(&self, screen: &Screen) -> bool {
        let now = Rest::at(screen, screen.grid().cursor);
        (now.cursor.row, now.cursor.col) == (self.cursor.row, self.cursor.col) && now.left == self.left
    }
}

/// Predicted edits over the authoritative grid, with Mosh's epoch rule.
///
/// **Epochs.** Anything that can change what the server does with input
/// starts a new epoch: Enter or any other key that is not predicted, a paste,
/// a mismatch or timeout, a resize, or output that left the cursor somewhere
/// else while nothing was predicted (a program printing a prompt; judged at
/// the next keystroke, so a shell's own redraw that comes back is not one). Predictions in an epoch
/// are tentative: tracked and reconciled, but **not drawn**, until the server
/// echoes the first of them. Then the epoch is confirmed and the rest of it is
/// drawn instantly.
///
/// This is what keeps a password off the screen. `sudo` prints its prompt
/// after an Enter, so typing into it starts a fresh epoch, and a prompt with
/// echo off never confirms one. The old guard, inferring echo-off from timed-out
/// predictions, could not do this alone: timeouts are noticed when output
/// arrives, and a password prompt sends none, so every character stayed drawn
/// until Enter. The live harness (app `tests/live_ssh.rs`) caught exactly that.
///
/// The cost is one round trip on the first keystroke of each line. Every
/// keystroke after it is instant.
pub struct Predictor {
    queue: VecDeque<Prediction>,
    stats: Stats,
    config: Config,
    /// The current epoch. Bumped by anything listed above.
    epoch: u64,
    /// The latest epoch the server has confirmed by echoing a prediction made
    /// in it. Predictions are drawn only while this equals `epoch`.
    confirmed_epoch: Option<u64>,
    /// Where the server's cursor rests while nothing is predicted, and the
    /// glyph just left of it: set by the confirmation that emptied the queue,
    /// or from the screen when nothing was predicted. The next keystroke
    /// compares against it (see [`Predictor::on_input`]).
    rest: Option<Rest>,
    /// The resting point a keystroke found the cursor away from. Its epoch
    /// is tentative, but if the cursor comes back there before anything else
    /// happens, the output was the shell's own redraw (the key arrived in the
    /// middle of it) and the epoch is trusted again without a round trip.
    suspect: Option<Rest>,
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
            confirmed_epoch: None,
            rest: None,
            suspect: None,
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
    /// Tentative predictions are not included: that is the point of them.
    pub fn overlay(&self) -> impl Iterator<Item = Overlay> + '_ {
        self.queue.iter().filter_map(|p| {
            if p.kind == Kind::CursorOnly || !p.shown {
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

    /// Where the cursor should be drawn, if drawn predictions have moved it.
    pub fn cursor(&self) -> Option<Cursor> {
        self.queue.iter().rev().find(|p| p.shown).map(|p| p.cursor_after)
    }

    /// Where the next prediction starts: after the last one, drawn or not.
    fn tail_cursor(&self) -> Option<Cursor> {
        self.queue.back().map(|p| p.cursor_after)
    }

    fn confident(&self) -> bool {
        self.confirmed_epoch == Some(self.epoch)
    }

    /// Start a new epoch: what is typed next stays hidden until the server
    /// echoes some of it.
    fn new_epoch(&mut self) {
        self.epoch += 1;
        self.suspect = None;
    }

    /// Input reached the server without passing through [`on_input`] — a
    /// paste. It may have changed anything, so start a new epoch.
    ///
    /// [`on_input`]: Predictor::on_input
    pub fn interrupt(&mut self) {
        self.new_epoch();
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
        if bytes.is_empty() {
            return 0;
        }
        // Output since the queue emptied that left the cursor somewhere else,
        // or changed the character beside it, was a program printing
        // something: perhaps a password prompt. Judged here, at the next
        // keystroke, not per read: a shell's own redraw (syntax highlighting,
        // autosuggestions, a clock in RPROMPT) wanders and comes back, and the
        // network can split it across reads anywhere.
        if self.queue.is_empty() {
            if let Some(r) = self.rest.take() {
                if !r.holds(screen) {
                    self.new_epoch();
                    if r.from_echo && screen.grid().cursor.row == r.cursor.row {
                        self.suspect = Some(r);
                    }
                }
            }
        }
        if !self.may_predict(screen, ctx) {
            // Whatever these bytes do, it was not predicted: assume it changed
            // how the server treats what comes next.
            self.new_epoch();
            return 0;
        }

        let mut made = 0;
        // Cursor position to predict from: the last prediction's; else where
        // the shell will be waiting if a redraw is still settling; else the
        // grid's.
        let mut cursor = self
            .tail_cursor()
            .or(self.suspect.map(|r| r.cursor))
            .unwrap_or(screen.grid().cursor);

        let mut i = 0;
        while i < bytes.len() {
            if self.queue.len() >= MAX_OUTSTANDING {
                // The bytes left over still reach the server. A printable key
                // the queue had no room for is an ordinary key, but anything
                // else in the remainder (an Enter at the end of a pasted or
                // broadcast line) must start a new epoch. Missing it let the
                // echo of the line's first characters vouch for a password
                // prompt that the Enter brought up: the live harness drew 7
                // of 9 secret characters before this check existed.
                if bytes[i..].iter().any(|b| !(0x20..=0x7E).contains(b)) {
                    self.new_epoch();
                }
                break;
            }
            // Every other `break` below is a key that is not predicted (Enter,
            // Tab, Ctrl-C, a wrap, a refused move): a new epoch.
            let shown = self.confident();
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
                        self.new_epoch();
                        break;
                    }
                    let p = Prediction {
                        epoch: self.epoch,
                        shown,
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
                        self.new_epoch();
                        break; // would need to predict a line join
                    }
                    let p = Prediction {
                        epoch: self.epoch,
                        shown,
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
                        self.new_epoch();
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
                            self.new_epoch();
                            break;
                        }
                        (cursor.col + 1, cursor.col)
                    } else {
                        if cursor.col == 0 {
                            self.new_epoch();
                            break;
                        }
                        (cursor.col - 1, cursor.col - 1)
                    };
                    if screen.grid().cell(cursor.row, over).0 == crate::grid::BLANK {
                        self.new_epoch();
                        break;
                    }
                    let p = Prediction {
                        epoch: self.epoch,
                        shown,
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
                _ => {
                    self.new_epoch();
                    break;
                }
            };

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
    ///
    /// Also call it with no new output, so timeouts fire on a link that has
    /// gone quiet (the app does, from its frame poll).
    pub fn reconcile(&mut self, screen: &Screen, now_ms: u64) {
        let cursor = screen.grid().cursor;
        // The cursor is back where the shell waits, with the same character
        // beside it: what moved it was a redraw, and this epoch can be drawn.
        if let Some(r) = self.suspect {
            if r.holds(screen) {
                self.suspect = None;
                self.confirmed_epoch = Some(self.epoch);
                let epoch = self.epoch;
                for q in self.queue.iter_mut().filter(|q| q.epoch == epoch) {
                    q.shown = true;
                }
            }
        }
        if self.queue.is_empty() {
            // Nothing predicted: remember where the cursor rests, once. Moves
            // after this are judged at the next keystroke.
            if self.rest.is_none() {
                self.rest = Some(Rest::at(screen, cursor));
            }
        } else {
            self.resolve(screen, cursor, now_ms);
        }
    }

    fn resolve(&mut self, screen: &Screen, cursor: Cursor, now_ms: u64) {
        while let Some(p) = self.queue.front().copied() {
            // Arrival: the server's cursor is exactly where some prediction k
            // left it, and every cell predictions 0..=k touch holds what they
            // predicted, applied in order. Confirm the run.
            //
            // Exact position, not "at or past": backspace and Left move the
            // cursor LEFT, so a server cursor that has gone further left has
            // already passed a target it has not reached yet. Three quick
            // backspaces used to confirm the first and then judge the second
            // against a cell not yet erased, flushing the queue (a visible
            // flicker) on every multi-character delete. The live harness found
            // it.
            if let Some(k) = self.arrived(screen, cursor) {
                for _ in 0..=k {
                    self.confirm(screen);
                }
                continue;
            }

            if p.kind == Kind::Print {
                // A printed character may also be confirmed by the server's
                // cursor having moved PAST it (more output after the echo).
                // Rightward only, so "past" is meaningful here.
                let (glyph, _) = screen.grid().cell(p.row, p.col);
                let server_reached =
                    (cursor.row, cursor.col) >= (p.cursor_after.row, p.cursor_after.col);
                if server_reached {
                    if glyph == p.glyph {
                        self.confirm(screen);
                        continue;
                    }
                    // The server produced something different. §5.5: discard
                    // ALL outstanding predictions and snap. Never partially
                    // repair.
                    self.flush(false);
                    return;
                }
            }

            // Not echoed yet.
            if now_ms.saturating_sub(p.sent_at_ms) >= PREDICTION_TIMEOUT_MS {
                // A cursor move the shell declined to make — Left at the start
                // of the input line, say — is NOT evidence of a password
                // prompt, so it must not feed the echo-off guess.
                if p.kind != Kind::CursorOnly {
                    self.silent_timeouts += 1;
                    if self.silent_timeouts >= SILENT_TIMEOUTS_BEFORE_ECHO_OFF {
                        // Nothing came back at all: treat as echo-off until
                        // proven otherwise.
                        self.echo_off = true;
                    }
                }
                self.flush(true);
                return;
            }
            break;
        }

        self.evaluate_hit_rate();
    }

    /// The first k such that the server cursor equals prediction k's
    /// `cursor_after` and the cells written by predictions 0..=k match the
    /// grid. O(k²) with k ≤ [`MAX_OUTSTANDING`] (12), no allocation.
    fn arrived(&self, screen: &Screen, cursor: Cursor) -> Option<usize> {
        let mut cells = [(0usize, 0usize, 0u32); MAX_OUTSTANDING];
        let mut n = 0;
        for (k, q) in self.queue.iter().enumerate() {
            let write = match q.kind {
                Kind::Print => Some(q.glyph),
                Kind::Backspace => Some(crate::grid::BLANK),
                Kind::CursorOnly => None,
            };
            if let Some(glyph) = write {
                // Last write to a cell wins: 'a' then Backspace leaves a blank.
                match cells[..n].iter_mut().find(|c| (c.0, c.1) == (q.row, q.col)) {
                    Some(c) => c.2 = glyph,
                    None if n < MAX_OUTSTANDING => {
                        cells[n] = (q.row, q.col, glyph);
                        n += 1;
                    }
                    None => return None,
                }
            }
            if (cursor.row, cursor.col) == (q.cursor_after.row, q.cursor_after.col)
                && cells[..n]
                    .iter()
                    .all(|&(r, c, g)| screen.grid().cell(r, c).0 == g)
            {
                return Some(k);
            }
        }
        None
    }

    fn confirm(&mut self, screen: &Screen) {
        let Some(p) = self.queue.pop_front() else { return };
        if self.queue.is_empty() {
            // The shell is now waiting where this echo left it, whatever it
            // draws around the cursor meanwhile.
            self.rest = Some(Rest { from_echo: true, ..Rest::at(screen, p.cursor_after) });
        }
        if p.shown {
            self.stats.confirmed += 1;
        } else {
            self.stats.withheld += 1;
        }
        self.silent_timeouts = 0;
        // A confirmed echo disproves the echo-off guess.
        self.echo_off = false;
        self.suspect = None;
        // The server echoed a keystroke from the current epoch: it is
        // echoing, so the rest of this epoch can be drawn. An echo from an
        // OLDER epoch proves nothing about this one — the classic case is a
        // character typed before Enter confirming after a password prompt
        // has appeared.
        if p.epoch == self.epoch {
            self.confirmed_epoch = Some(self.epoch);
            for q in self.queue.iter_mut().filter(|q| q.epoch == p.epoch) {
                q.shown = true;
            }
        }
    }

    /// Discard everything outstanding and start a new epoch. With
    /// `front_timed_out`, the oldest prediction is scored as a timeout rather
    /// than a miss. Tentative predictions are scored as neither (see [`Stats`]).
    fn flush(&mut self, front_timed_out: bool) {
        for (i, p) in self.queue.iter().enumerate() {
            if !p.shown {
                self.stats.withheld += 1;
            } else if i == 0 && front_timed_out {
                self.stats.timed_out += 1;
            } else {
                self.stats.missed += 1;
            }
        }
        self.queue.clear();
        self.stats.flushes += 1;
        self.rest = None;
        self.new_epoch();
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
        self.rest = None;
        self.new_epoch();
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

    /// A predictor whose current epoch the server has already confirmed: the
    /// state of a session mid-typing, which is what most tests here are about.
    /// The epoch tests below start from `Predictor::new` instead.
    fn confident(mut p: Predictor) -> Predictor {
        p.confirmed_epoch = Some(p.epoch);
        p
    }

    fn setup() -> (Screen, Parser, Predictor) {
        (
            Screen::new(10, 40),
            Parser::new(),
            confident(Predictor::new(Config::default())),
        )
    }

    /* ---- Epochs: what must never be drawn ------------------------------- */

    #[test]
    fn a_fresh_session_draws_nothing_until_the_server_echoes() {
        let mut screen = Screen::new(10, 40);
        let mut parser = Parser::new();
        let mut pred = Predictor::new(Config::default());

        assert_eq!(pred.on_input(&screen, &Context::default(), b"ls", 0), 2);
        assert!(pred.overlay().next().is_none(), "tentative predictions were drawn");
        assert_eq!(pred.cursor(), None);

        // The server echoes the first: the epoch is confirmed and the rest of
        // it appears at once.
        feed(&mut screen, &mut parser, b"l");
        pred.reconcile(&screen, 1);
        let o: Vec<Overlay> = pred.overlay().collect();
        assert_eq!(o.len(), 1);
        assert_eq!(o[0].glyph, b's' as u32);
        assert_eq!(pred.cursor(), Some(Cursor { row: 0, col: 2 }));
        // Neither a hit nor a miss: the 'l' was never on screen.
        assert_eq!((pred.stats().confirmed, pred.stats().withheld), (0, 1));
    }

    /// The live-harness bug: `read -s` showed every character typed into it.
    #[test]
    fn a_password_typed_after_enter_is_never_drawn() {
        let (mut screen, mut parser, mut pred) = setup();
        feed(&mut screen, &mut parser, b"$ ");
        pred.reconcile(&screen, 0);

        pred.on_input(&screen, &Context::default(), b"sudo -v", 0);
        feed(&mut screen, &mut parser, b"sudo -v");
        pred.reconcile(&screen, 1);
        assert_eq!(pred.outstanding(), 0);

        // Enter, then the prompt arrives. Echo is off from here.
        pred.on_input(&screen, &Context::default(), b"\r", 2);
        feed(&mut screen, &mut parser, b"\r\n[sudo] password: ");
        pred.reconcile(&screen, 3);

        for (i, c) in b"hunter2".iter().enumerate() {
            let t = 10 + i as u64;
            assert_eq!(pred.on_input(&screen, &Context::default(), &[*c], t), 1);
            assert!(pred.overlay().next().is_none(), "secret character {i} drawn");
            assert_eq!(pred.cursor(), None, "the caret moved for a hidden character");
            pred.reconcile(&screen, t);
        }
        // And the timeouts that eventually fire do not score as misses.
        pred.reconcile(&screen, 10_000);
        assert_eq!(pred.stats().missed + pred.stats().timed_out, 0);
        assert!(pred.overlay().next().is_none());
    }

    /// The prompt can arrive with no Enter in this terminal: a program that
    /// reads one key, then asks for a password. Output that moves the cursor
    /// while nothing is predicted starts a new epoch.
    #[test]
    fn output_that_moves_the_cursor_unprompted_starts_a_new_epoch() {
        let (mut screen, mut parser, mut pred) = setup();
        pred.reconcile(&screen, 0);
        feed(&mut screen, &mut parser, b"Password: ");
        pred.reconcile(&screen, 1);
        pred.on_input(&screen, &Context::default(), b"x", 2);
        assert!(pred.overlay().next().is_none());
    }

    /// The other side of the previous rule: a prompt redraw that puts the
    /// cursor back where it was (a clock in zsh's RPROMPT) is not a new prompt,
    /// and must not cost the next keystroke a round trip.
    #[test]
    fn output_that_leaves_the_cursor_in_place_keeps_the_epoch() {
        let (mut screen, mut parser, mut pred) = setup();
        feed(&mut screen, &mut parser, b"$ ");
        pred.reconcile(&screen, 0);
        feed(&mut screen, &mut parser, b"\x1b7\x1b[1;30H12:00\x1b8");
        pred.reconcile(&screen, 1);
        pred.on_input(&screen, &Context::default(), b"x", 2);
        assert_eq!(pred.overlay().count(), 1);
    }

    /// zsh-syntax-highlighting and zsh-autosuggestions answer every key with
    /// the echo, a rewrite of it, a grey suggestion after the cursor, then a
    /// move back to the insertion point. The network splits that wherever it
    /// likes; the move back arriving in a read of its own is not a new prompt.
    /// Before this held, a third of keystrokes over a 200 ms link waited a
    /// round trip (the app's latency probe, with Kali's default zshrc).
    #[test]
    fn a_redraw_split_across_reads_keeps_the_epoch() {
        let (mut screen, mut parser, mut pred) = setup();
        feed(&mut screen, &mut parser, b"$ ");
        pred.reconcile(&screen, 0);
        assert_eq!(pred.on_input(&screen, &Context::default(), b"x", 1), 1);
        // The echo, its highlighted rewrite, and the suggestion.
        feed(&mut screen, &mut parser, b"x\x08\x1b[4mx\x1b[24m\x1b[38;5;244margs -n1\x1b[39m");
        pred.reconcile(&screen, 2);
        // A later read: back to the insertion point.
        feed(&mut screen, &mut parser, b"\x1b[8D");
        pred.reconcile(&screen, 3);
        pred.on_input(&screen, &Context::default(), b"y", 4);
        assert!(
            pred.overlay().any(|o| o.glyph == b'y' as u32),
            "the next key waited for the server after a split redraw"
        );
    }

    /// The next key can arrive between the two halves of that redraw: after
    /// the echo confirmed, before the move back. It must be predicted where the
    /// shell will be waiting (not where the cursor happens to be mid-redraw),
    /// held back, and shown as soon as the move back lands.
    #[test]
    fn a_key_typed_mid_redraw_is_shown_when_the_redraw_settles() {
        let (mut screen, mut parser, mut pred) = setup();
        feed(&mut screen, &mut parser, b"$ ");
        pred.reconcile(&screen, 0);
        pred.on_input(&screen, &Context::default(), b"x", 1);
        feed(&mut screen, &mut parser, b"x\x08\x1b[4mx\x1b[24m\x1b[38;5;244margs -n1\x1b[39m");
        pred.reconcile(&screen, 2);
        // 'y' arrives with the cursor still out at the end of the suggestion.
        assert_eq!(pred.on_input(&screen, &Context::default(), b"y", 3), 1);
        assert!(pred.overlay().next().is_none(), "drawn before the shell was back where it waits");
        feed(&mut screen, &mut parser, b"\x1b[8D");
        pred.reconcile(&screen, 4);
        let shown: Vec<_> = pred.overlay().collect();
        assert_eq!(shown.len(), 1, "not shown once the redraw settled");
        assert_eq!((shown[0].glyph, shown[0].col), (b'y' as u32, 3), "predicted at the wrong column");
        // And the server's echo of it confirms it normally.
        feed(&mut screen, &mut parser, b"y\x1b[38;5;244mes\x1b[39m\x1b[2D");
        pred.reconcile(&screen, 5);
        assert_eq!(pred.outstanding(), 0);
        assert_eq!(pred.stats().flushes, 0);
    }

    /// The same interleaving where the output was a prompt, not a redraw: the
    /// cursor never comes back, so the held key is never shown.
    #[test]
    fn a_key_typed_while_a_prompt_prints_stays_hidden() {
        let (mut screen, mut parser, mut pred) = setup();
        feed(&mut screen, &mut parser, b"continue? ");
        pred.reconcile(&screen, 0);
        pred.on_input(&screen, &Context::default(), b"y", 1);
        feed(&mut screen, &mut parser, b"y\r\nPassword: ");
        pred.reconcile(&screen, 2);
        pred.on_input(&screen, &Context::default(), b"s", 3);
        pred.reconcile(&screen, 4);
        pred.on_input(&screen, &Context::default(), b"e", 5);
        pred.reconcile(&screen, 6);
        assert!(pred.overlay().next().is_none(), "a character typed at a password prompt was drawn");
    }

    /// The guard the previous test must not weaken: output that leaves the
    /// cursor somewhere new after a confirmed echo, in however many reads, is
    /// a program printing something (`read -n1`, then a password prompt).
    #[test]
    fn a_prompt_printed_after_a_confirmed_echo_hides_what_follows() {
        let (mut screen, mut parser, mut pred) = setup();
        feed(&mut screen, &mut parser, b"continue? ");
        pred.reconcile(&screen, 0);
        pred.on_input(&screen, &Context::default(), b"y", 1);
        feed(&mut screen, &mut parser, b"y");
        pred.reconcile(&screen, 2);
        feed(&mut screen, &mut parser, b"\r\nPass");
        pred.reconcile(&screen, 3);
        feed(&mut screen, &mut parser, b"word: ");
        pred.reconcile(&screen, 4);
        pred.on_input(&screen, &Context::default(), b"s", 5);
        assert!(pred.overlay().next().is_none(), "a character typed at a password prompt was drawn");
    }

    /// Same, with the prompt overwriting the echoed line so the cursor lands
    /// exactly where the echo left it. The echoed character is gone, so this
    /// is still not the shell's own redraw.
    #[test]
    fn a_prompt_that_lands_on_the_same_column_still_hides_what_follows() {
        let (mut screen, mut parser, mut pred) = setup();
        feed(&mut screen, &mut parser, b"continue?");
        pred.reconcile(&screen, 0);
        pred.on_input(&screen, &Context::default(), b"y", 1);
        feed(&mut screen, &mut parser, b"y");
        pred.reconcile(&screen, 2);
        // "continue?y" leaves the cursor at col 10; so does "\rPassword: ".
        feed(&mut screen, &mut parser, b"\rPassword: ");
        pred.reconcile(&screen, 3);
        assert_eq!(screen.grid().cursor.col, 10);
        pred.on_input(&screen, &Context::default(), b"s", 4);
        assert!(pred.overlay().next().is_none(), "a character typed at a password prompt was drawn");
    }

    /// An echo from before Enter must not vouch for what is typed after it.
    #[test]
    fn an_echo_from_an_older_epoch_reveals_nothing_newer() {
        let (mut screen, mut parser, mut pred) = setup();
        pred.on_input(&screen, &Context::default(), b"a", 0); // drawn
        pred.on_input(&screen, &Context::default(), b"\r", 1); // new epoch
        pred.on_input(&screen, &Context::default(), b"p", 2); // tentative
        // The server finally echoes the 'a'.
        feed(&mut screen, &mut parser, b"a");
        pred.reconcile(&screen, 3);
        assert!(
            pred.overlay().all(|o| o.glyph != b'p' as u32),
            "a keystroke typed after Enter was revealed by an echo from before it"
        );
    }

    /// A whole command line in one write (a snippet, broadcast input) is
    /// longer than the queue. The Enter at its end is past the cap, and must
    /// still start a new epoch.
    #[test]
    fn an_enter_past_the_queue_cap_still_starts_a_new_epoch() {
        let (mut screen, mut parser, mut pred) = setup();
        feed(&mut screen, &mut parser, b"$ ");
        pred.reconcile(&screen, 0);
        let line = b"read -s -p 'pw: ' x; echo\r";
        assert!(line.len() > MAX_OUTSTANDING + 1);
        assert_eq!(pred.on_input(&screen, &Context::default(), line, 1), MAX_OUTSTANDING);

        // The echo and the prompt in ONE chunk, as they arrived in the live
        // run. Split in two, the "output moved the cursor" rule would start
        // a new epoch anyway and this test would prove nothing.
        feed(&mut screen, &mut parser, b"read -s -p 'pw: ' x; echo\r\npw: ");
        pred.reconcile(&screen, 2);
        pred.on_input(&screen, &Context::default(), b"hunter2", 4);
        assert!(pred.overlay().next().is_none(), "a password was drawn");
    }

    /// A paste bypasses `on_input`, but it can contain Enter.
    #[test]
    fn a_paste_starts_a_new_epoch() {
        let (screen, _p, mut pred) = setup();
        pred.interrupt();
        pred.on_input(&screen, &Context::default(), b"x", 0);
        assert!(pred.overlay().next().is_none());
    }

    /// Timeouts must fire on a quiet link: the app reconciles from its frame
    /// poll with no new output, and a drawn prediction that never echoes is
    /// removed then, not when the next output happens to arrive.
    #[test]
    fn a_drawn_prediction_expires_without_any_output() {
        let (screen, _p, mut pred) = setup();
        pred.on_input(&screen, &Context::default(), b"a", 0);
        assert_eq!(pred.overlay().count(), 1);
        pred.reconcile(&screen, PREDICTION_TIMEOUT_MS - 1);
        assert_eq!(pred.overlay().count(), 1);
        pred.reconcile(&screen, PREDICTION_TIMEOUT_MS);
        assert_eq!(pred.overlay().count(), 0);
        assert_eq!(pred.stats().timed_out, 1);
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

    /// The live-harness bug: several backspaces whose echoes arrive one by
    /// one. After the first echo the server cursor is LEFT of the second
    /// backspace's start, which an "at or past" rule reads as already reached.
    #[test]
    fn rapid_backspaces_confirm_one_by_one_without_a_flush() {
        let (mut screen, mut parser, mut pred) = setup();
        feed(&mut screen, &mut parser, b"$ fox");
        pred.reconcile(&screen, 0);
        assert_eq!(pred.on_input(&screen, &Context::default(), b"\x7f\x7f\x7f", 1), 3);
        for t in 2..5 {
            feed(&mut screen, &mut parser, b"\x08\x1b[K"); // readline's echo
            pred.reconcile(&screen, t);
        }
        assert_eq!(pred.outstanding(), 0);
        assert_eq!((pred.stats().confirmed, pred.stats().missed), (3, 0));
        assert_eq!(pred.stats().flushes, 0);
    }

    #[test]
    fn rapid_backspaces_echoed_in_one_chunk_confirm_together() {
        let (mut screen, mut parser, mut pred) = setup();
        feed(&mut screen, &mut parser, b"$ fox");
        pred.reconcile(&screen, 0);
        pred.on_input(&screen, &Context::default(), b"\x7f\x7f\x7f", 1);
        feed(&mut screen, &mut parser, b"\x08 \x08\x08 \x08\x08 \x08");
        pred.reconcile(&screen, 2);
        assert_eq!((pred.stats().confirmed, pred.stats().missed), (3, 0));
    }

    /// A character and the backspace that deletes it, echoed together: the
    /// cell ends blank and the cursor back where it started.
    #[test]
    fn a_character_and_its_backspace_echoed_together_both_confirm() {
        let (mut screen, mut parser, mut pred) = setup();
        feed(&mut screen, &mut parser, b"$ ");
        pred.reconcile(&screen, 0);
        pred.on_input(&screen, &Context::default(), b"a\x7f", 1);
        feed(&mut screen, &mut parser, b"a\x08\x1b[K");
        pred.reconcile(&screen, 2);
        assert_eq!((pred.stats().confirmed, pred.stats().missed), (2, 0));
    }

    /// The safety side of exact-position arrival: a coincidental cursor match
    /// with the cells still wrong is neither a confirm nor a mismatch. The
    /// frame-poll reconcile runs before any echo, and must not flush here.
    #[test]
    fn a_cursor_coincidence_before_the_echo_is_not_a_verdict() {
        let (mut screen, mut parser, mut pred) = setup();
        feed(&mut screen, &mut parser, b"$ fox");
        pred.reconcile(&screen, 0);
        // Backspace then 'y': the cursor ends where it started, but the cell
        // still holds 'x'. (Retyping the same 'x' would predict the screen as
        // it already is, and confirming that early draws nothing different.)
        pred.on_input(&screen, &Context::default(), b"\x7fy", 1);
        pred.reconcile(&screen, 2); // no output yet
        assert_eq!(pred.outstanding(), 2);
        assert_eq!(pred.stats().flushes, 0);
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

        // Only the first was drawn (the timeout starts a new epoch); every
        // later one was tentative. All of them expired.
        let st = pred.stats();
        assert!(st.timed_out + st.withheld > SILENT_TIMEOUTS_BEFORE_ECHO_OFF);
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
        let withheld = pred.stats().withheld;
        pred.on_input(&screen, &Context::default(), b"x", 100_000);
        feed(&mut screen, &mut parser, b"x");
        pred.reconcile(&screen, 100_001);
        assert!(!pred.echo_appears_disabled());
        // Tentative (a timeout started this epoch), so withheld, not a hit;
        // but the echo confirmed the epoch, so the next key is drawn.
        assert_eq!(pred.stats().withheld, withheld + 1);
        pred.on_input(&screen, &Context::default(), b"y", 100_002);
        assert_eq!(pred.overlay().count(), 1);
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
            // Each mismatch starts a new epoch, so first get one confirmed:
            // only a drawn prediction that misses counts against the link.
            pred.on_input(&screen, &Context::default(), b"b", i * 10);
            feed(&mut screen, &mut parser, b"b");
            pred.reconcile(&screen, i * 10 + 1);
            pred.on_input(&screen, &Context::default(), b"a", i * 10 + 2);
            feed(&mut screen, &mut parser, b"Z");
            pred.reconcile(&screen, i * 10 + 3);
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
                // A fresh session: the first is withheld, the rest drawn.
                prop_assert_eq!(pred.stats().confirmed + pred.stats().withheld, made as u32);
                prop_assert_eq!(pred.stats().withheld, 1);
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
