//! VT500 parser — table-driven DFA with an ASCII bulk path.
//!
//! CLAUDE.md §5.2: "Implement Paul Williams' VT500 parser as a **static
//! transition table** `[[State; 256]; N_STATES]` — one indexed load and one
//! branch per byte, no string matching, no allocation."
//!
//! The table is built by a `const fn`, so it is materialised at compile time and
//! lands in `.rodata`: 14 states x 256 bytes x 2 = 7 KB, hot in L1 during a
//! flood.
//!
//! Two deliberate departures from the 1990s spec, both because this is a UTF-8
//! terminal in 2026:
//!
//! 1. **C1 controls (0x80–0x9F) are not interpreted.** In UTF-8, those bytes are
//!    continuation bytes. Honouring the classic "anywhere" transitions would make
//!    the second byte of `œ` (0x9C) terminate a string, corrupting any output
//!    containing non-ASCII text. Every modern terminal disables C1 in UTF-8 mode.
//! 2. **Colon is a parameter separator, not an error.** The classic table sends
//!    `:` to CSI-ignore, which would discard `SGR 38:2::R:G:B` — the ITU-T T.416
//!    truecolour form that is now common. Treating `:` like `;` parses it into
//!    the same flat parameter list, so an SGR handler needs no special case.

use crate::params::Params;

/* ---- States and actions --------------------------------------------------
Plain `u8` consts rather than enums: this table is built in a `const fn`, and
integers keep that code simple enough to read against the spec. */

pub(crate) mod state {
    pub const GROUND: u8 = 0;
    pub const ESCAPE: u8 = 1;
    pub const ESCAPE_INTERMEDIATE: u8 = 2;
    pub const CSI_ENTRY: u8 = 3;
    pub const CSI_PARAM: u8 = 4;
    pub const CSI_INTERMEDIATE: u8 = 5;
    pub const CSI_IGNORE: u8 = 6;
    pub const DCS_ENTRY: u8 = 7;
    pub const DCS_PARAM: u8 = 8;
    pub const DCS_INTERMEDIATE: u8 = 9;
    pub const DCS_PASSTHROUGH: u8 = 10;
    pub const DCS_IGNORE: u8 = 11;
    pub const OSC_STRING: u8 = 12;
    pub const SOS_PM_APC: u8 = 13;
    pub const COUNT: usize = 14;
}

mod action {
    pub const NONE: u8 = 0;
    pub const IGNORE: u8 = 1;
    pub const PRINT: u8 = 2;
    pub const EXECUTE: u8 = 3;
    pub const COLLECT: u8 = 4;
    pub const PARAM: u8 = 5;
    pub const ESC_DISPATCH: u8 = 6;
    pub const CSI_DISPATCH: u8 = 7;
    pub const PUT: u8 = 8;
    pub const OSC_PUT: u8 = 9;
}

#[inline]
const fn cell(action: u8, next: u8) -> u16 {
    ((action as u16) << 8) | next as u16
}

#[inline]
const fn act_of(c: u16) -> u8 {
    (c >> 8) as u8
}

#[inline]
const fn state_of(c: u16) -> u8 {
    c as u8
}

/// Fill an inclusive byte range. Takes and returns by value so this stays a
/// plain `const fn` with no `&mut` requirement.
const fn fill(mut row: [u16; 256], lo: u8, hi: u8, a: u8, s: u8) -> [u16; 256] {
    let mut b = lo as usize;
    while b <= hi as usize {
        row[b] = cell(a, s);
        b += 1;
    }
    row
}

/// The C0 controls every state executes or ignores identically.
const fn c0(row: [u16; 256], a: u8, s: u8) -> [u16; 256] {
    let row = fill(row, 0x00, 0x17, a, s);
    let row = fill(row, 0x19, 0x19, a, s);
    fill(row, 0x1C, 0x1F, a, s)
}

/// Transitions valid from any state. Applied last so they win.
const fn anywhere(row: [u16; 256]) -> [u16; 256] {
    // CAN and SUB abort whatever is in flight.
    let row = fill(row, 0x18, 0x18, action::EXECUTE, state::GROUND);
    let row = fill(row, 0x1A, 0x1A, action::EXECUTE, state::GROUND);
    // ESC always restarts a sequence.
    fill(row, 0x1B, 0x1B, action::NONE, state::ESCAPE)
}

const fn build_table() -> [[u16; 256]; state::COUNT] {
    let mut t = [[0u16; 256]; state::COUNT];

    // --- Ground -----------------------------------------------------------
    // 0x80..=0xFF never reach the table in Ground: they are routed to the UTF-8
    // collector before lookup.
    let mut r = [cell(action::PRINT, state::GROUND); 256];
    r = c0(r, action::EXECUTE, state::GROUND);
    r = fill(r, 0x7F, 0x7F, action::IGNORE, state::GROUND);
    t[state::GROUND as usize] = anywhere(r);

    // --- Escape -----------------------------------------------------------
    let mut r = [cell(action::ESC_DISPATCH, state::GROUND); 256];
    r = c0(r, action::EXECUTE, state::ESCAPE);
    r = fill(r, 0x20, 0x2F, action::COLLECT, state::ESCAPE_INTERMEDIATE);
    r = fill(r, 0x50, 0x50, action::NONE, state::DCS_ENTRY); // ESC P
    r = fill(r, 0x58, 0x58, action::NONE, state::SOS_PM_APC); // ESC X (SOS)
    r = fill(r, 0x5B, 0x5B, action::NONE, state::CSI_ENTRY); // ESC [
    r = fill(r, 0x5D, 0x5D, action::NONE, state::OSC_STRING); // ESC ]
    r = fill(r, 0x5E, 0x5F, action::NONE, state::SOS_PM_APC); // ESC ^ (PM), ESC _ (APC)
    r = fill(r, 0x7F, 0x7F, action::IGNORE, state::ESCAPE);
    t[state::ESCAPE as usize] = anywhere(r);

    // --- Escape intermediate ---------------------------------------------
    let mut r = [cell(action::ESC_DISPATCH, state::GROUND); 256];
    r = c0(r, action::EXECUTE, state::ESCAPE_INTERMEDIATE);
    r = fill(r, 0x20, 0x2F, action::COLLECT, state::ESCAPE_INTERMEDIATE);
    r = fill(r, 0x7F, 0x7F, action::IGNORE, state::ESCAPE_INTERMEDIATE);
    t[state::ESCAPE_INTERMEDIATE as usize] = anywhere(r);

    // --- CSI entry --------------------------------------------------------
    let mut r = [cell(action::IGNORE, state::CSI_IGNORE); 256];
    r = c0(r, action::EXECUTE, state::CSI_ENTRY);
    r = fill(r, 0x20, 0x2F, action::COLLECT, state::CSI_INTERMEDIATE);
    r = fill(r, 0x30, 0x39, action::PARAM, state::CSI_PARAM);
    r = fill(r, 0x3A, 0x3B, action::PARAM, state::CSI_PARAM); // ':' — see header
    r = fill(r, 0x3C, 0x3F, action::COLLECT, state::CSI_PARAM);
    r = fill(r, 0x40, 0x7E, action::CSI_DISPATCH, state::GROUND);
    r = fill(r, 0x7F, 0x7F, action::IGNORE, state::CSI_ENTRY);
    t[state::CSI_ENTRY as usize] = anywhere(r);

    // --- CSI param --------------------------------------------------------
    let mut r = [cell(action::IGNORE, state::CSI_IGNORE); 256];
    r = c0(r, action::EXECUTE, state::CSI_PARAM);
    r = fill(r, 0x20, 0x2F, action::COLLECT, state::CSI_INTERMEDIATE);
    r = fill(r, 0x30, 0x39, action::PARAM, state::CSI_PARAM);
    r = fill(r, 0x3A, 0x3B, action::PARAM, state::CSI_PARAM);
    r = fill(r, 0x3C, 0x3F, action::IGNORE, state::CSI_IGNORE);
    r = fill(r, 0x40, 0x7E, action::CSI_DISPATCH, state::GROUND);
    r = fill(r, 0x7F, 0x7F, action::IGNORE, state::CSI_PARAM);
    t[state::CSI_PARAM as usize] = anywhere(r);

    // --- CSI intermediate -------------------------------------------------
    let mut r = [cell(action::IGNORE, state::CSI_IGNORE); 256];
    r = c0(r, action::EXECUTE, state::CSI_INTERMEDIATE);
    r = fill(r, 0x20, 0x2F, action::COLLECT, state::CSI_INTERMEDIATE);
    r = fill(r, 0x30, 0x3F, action::IGNORE, state::CSI_IGNORE);
    r = fill(r, 0x40, 0x7E, action::CSI_DISPATCH, state::GROUND);
    r = fill(r, 0x7F, 0x7F, action::IGNORE, state::CSI_INTERMEDIATE);
    t[state::CSI_INTERMEDIATE as usize] = anywhere(r);

    // --- CSI ignore -------------------------------------------------------
    let mut r = [cell(action::IGNORE, state::CSI_IGNORE); 256];
    r = c0(r, action::EXECUTE, state::CSI_IGNORE);
    r = fill(r, 0x40, 0x7E, action::NONE, state::GROUND);
    t[state::CSI_IGNORE as usize] = anywhere(r);

    // --- DCS entry --------------------------------------------------------
    let mut r = [cell(action::IGNORE, state::DCS_IGNORE); 256];
    r = c0(r, action::IGNORE, state::DCS_ENTRY);
    r = fill(r, 0x20, 0x2F, action::COLLECT, state::DCS_INTERMEDIATE);
    r = fill(r, 0x30, 0x39, action::PARAM, state::DCS_PARAM);
    r = fill(r, 0x3A, 0x3B, action::PARAM, state::DCS_PARAM);
    r = fill(r, 0x3C, 0x3F, action::COLLECT, state::DCS_PARAM);
    r = fill(r, 0x40, 0x7E, action::NONE, state::DCS_PASSTHROUGH);
    r = fill(r, 0x7F, 0x7F, action::IGNORE, state::DCS_ENTRY);
    t[state::DCS_ENTRY as usize] = anywhere(r);

    // --- DCS param --------------------------------------------------------
    let mut r = [cell(action::IGNORE, state::DCS_IGNORE); 256];
    r = c0(r, action::IGNORE, state::DCS_PARAM);
    r = fill(r, 0x20, 0x2F, action::COLLECT, state::DCS_INTERMEDIATE);
    r = fill(r, 0x30, 0x39, action::PARAM, state::DCS_PARAM);
    r = fill(r, 0x3A, 0x3B, action::PARAM, state::DCS_PARAM);
    r = fill(r, 0x3C, 0x3F, action::IGNORE, state::DCS_IGNORE);
    r = fill(r, 0x40, 0x7E, action::NONE, state::DCS_PASSTHROUGH);
    r = fill(r, 0x7F, 0x7F, action::IGNORE, state::DCS_PARAM);
    t[state::DCS_PARAM as usize] = anywhere(r);

    // --- DCS intermediate -------------------------------------------------
    let mut r = [cell(action::IGNORE, state::DCS_IGNORE); 256];
    r = c0(r, action::IGNORE, state::DCS_INTERMEDIATE);
    r = fill(r, 0x20, 0x2F, action::COLLECT, state::DCS_INTERMEDIATE);
    r = fill(r, 0x30, 0x3F, action::IGNORE, state::DCS_IGNORE);
    r = fill(r, 0x40, 0x7E, action::NONE, state::DCS_PASSTHROUGH);
    r = fill(r, 0x7F, 0x7F, action::IGNORE, state::DCS_INTERMEDIATE);
    t[state::DCS_INTERMEDIATE as usize] = anywhere(r);

    // --- DCS passthrough --------------------------------------------------
    let mut r = [cell(action::PUT, state::DCS_PASSTHROUGH); 256];
    r = c0(r, action::PUT, state::DCS_PASSTHROUGH);
    r = fill(r, 0x7F, 0x7F, action::IGNORE, state::DCS_PASSTHROUGH);
    t[state::DCS_PASSTHROUGH as usize] = anywhere(r);

    // --- DCS ignore -------------------------------------------------------
    let r = [cell(action::IGNORE, state::DCS_IGNORE); 256];
    t[state::DCS_IGNORE as usize] = anywhere(r);

    // --- OSC string -------------------------------------------------------
    // 0x80..=0xFF are OSC_PUT: window titles are UTF-8 and must survive intact.
    let mut r = [cell(action::OSC_PUT, state::OSC_STRING); 256];
    r = c0(r, action::IGNORE, state::OSC_STRING);
    r = fill(r, 0x07, 0x07, action::NONE, state::GROUND); // BEL terminates
    t[state::OSC_STRING as usize] = anywhere(r);

    // --- SOS/PM/APC -------------------------------------------------------
    let r = [cell(action::IGNORE, state::SOS_PM_APC); 256];
    t[state::SOS_PM_APC as usize] = anywhere(r);

    t
}

static TABLE: [[u16; 256]; state::COUNT] = build_table();

/* ---- The ASCII bulk path (§5.2) ------------------------------------------ */

const ONES: u64 = 0x0101_0101_0101_0101;
const HIGH: u64 = 0x8080_8080_8080_8080;

/// Length of the leading run of printable ASCII (`0x20..=0x7E`).
///
/// §5.2: "Real terminal output is >95% this path, and this is where most of the
/// throughput multiple lives."
///
/// SWAR rather than `memchr`, because the predicate is a *range* and memchr
/// searches for specific bytes. Eight bytes are tested per iteration with two
/// classic bit tricks; on the word that contains a control byte we fall back to
/// a byte loop, since the borrow in `hasless` can set bits in lanes after the
/// first offender and locating it exactly is not worth the extra work.
#[inline]
pub fn ascii_run_len(bytes: &[u8]) -> usize {
    let mut i = 0;

    while i + 8 <= bytes.len() {
        // SAFETY-free: try_into on an exact-size slice cannot fail.
        let w = u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
        let has_ctrl = w.wrapping_sub(ONES * 0x20) & !w & HIGH; // any byte < 0x20
        let has_high = (w.wrapping_add(ONES) | w) & HIGH; // any byte > 0x7E
        if has_ctrl | has_high != 0 {
            break;
        }
        i += 8;
    }

    while i < bytes.len() {
        if !(0x20..0x7F).contains(&bytes[i]) {
            break;
        }
        i += 1;
    }
    i
}

/* ---- Perform -------------------------------------------------------------- */

/// Actions the parser hands to the screen.
///
/// `print_ascii` exists so the bulk path can hand over a whole run: dispatching
/// per character would throw away most of what the fast scan just bought.
pub trait Perform {
    fn print(&mut self, c: char);

    fn print_ascii(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.print(b as char);
        }
    }

    fn execute(&mut self, byte: u8);
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: u8);
    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8);
    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool);

    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: u8) {}
    fn put(&mut self, _byte: u8) {}
    fn unhook(&mut self) {}
}

const MAX_INTERMEDIATES: usize = 2;
/// Bounded so a hostile or broken stream cannot grow this without limit. A real
/// OSC 0 (window title) or OSC 52 (clipboard) fits comfortably; anything longer
/// is truncated rather than allowed to consume memory.
const MAX_OSC: usize = 4096;

pub struct Parser {
    state: u8,
    params: Params,
    intermediates: [u8; MAX_INTERMEDIATES],
    intermediate_len: usize,
    /// Set when a sequence overflowed its limits; the dispatch still fires so
    /// the screen can resynchronise, but with `ignore = true`.
    ignoring: bool,
    osc_raw: Vec<u8>,
    osc_ends: Vec<usize>,

    /// Incremental UTF-8 decode, so a multi-byte character split across two
    /// reads from the PTY is not corrupted.
    utf8_buf: [u8; 4],
    utf8_len: usize,
    utf8_needed: usize,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    pub fn new() -> Self {
        Parser {
            state: state::GROUND,
            params: Params::default(),
            intermediates: [0; MAX_INTERMEDIATES],
            intermediate_len: 0,
            ignoring: false,
            osc_raw: Vec::with_capacity(128),
            osc_ends: Vec::with_capacity(8),
            utf8_buf: [0; 4],
            utf8_len: 0,
            utf8_needed: 0,
        }
    }

    /// True while an OSC or DCS string is open.
    ///
    /// CLAUDE.md §5.5 lists "an OSC/DCS sequence is open" as a hard gate on
    /// predictive echo. Exposed here so the prediction layer can ask rather than
    /// keep a duplicate parser state that could drift.
    #[inline]
    pub fn in_string(&self) -> bool {
        matches!(
            self.state,
            state::OSC_STRING
                | state::DCS_PASSTHROUGH
                | state::DCS_ENTRY
                | state::DCS_PARAM
                | state::DCS_INTERMEDIATE
                | state::DCS_IGNORE
                | state::SOS_PM_APC
        )
    }

    #[inline]
    pub fn in_ground(&self) -> bool {
        self.state == state::GROUND && self.utf8_needed == 0
    }

    pub fn advance<P: Perform>(&mut self, perform: &mut P, bytes: &[u8]) {
        let mut i = 0;
        while i < bytes.len() {
            // Fast path first: in Ground with no partial UTF-8 pending, take the
            // longest printable-ASCII run in one go.
            if self.state == state::GROUND && self.utf8_needed == 0 {
                let run = ascii_run_len(&bytes[i..]);
                if run != 0 {
                    perform.print_ascii(&bytes[i..i + run]);
                    i += run;
                    continue;
                }
            }
            let b = bytes[i];
            i += 1;
            self.step(perform, b);
        }
    }

    fn step<P: Perform>(&mut self, perform: &mut P, b: u8) {
        // Non-ASCII in Ground belongs to the UTF-8 decoder, not the table.
        if self.state == state::GROUND && (b >= 0x80 || self.utf8_needed > 0) {
            self.utf8_step(perform, b);
            return;
        }

        let entry = TABLE[self.state as usize][b as usize];
        let action = act_of(entry);
        let next = state_of(entry);

        if next != self.state {
            self.exit(perform, self.state);
            self.perform_action(perform, action, b);
            self.enter(perform, next);
            self.state = next;
        } else {
            self.perform_action(perform, action, b);
        }
    }

    fn enter<P: Perform>(&mut self, perform: &mut P, s: u8) {
        match s {
            state::ESCAPE | state::CSI_ENTRY | state::DCS_ENTRY => self.clear(),
            state::OSC_STRING => {
                self.osc_raw.clear();
                self.osc_ends.clear();
            }
            state::DCS_PASSTHROUGH => {
                let ignore = self.ignoring;
                let inter = self.intermediates[..self.intermediate_len].to_vec();
                perform.hook(&self.params, &inter, ignore, 0);
            }
            _ => {}
        }
    }

    fn exit<P: Perform>(&mut self, perform: &mut P, s: u8) {
        match s {
            state::OSC_STRING => self.osc_end(perform, false),
            state::DCS_PASSTHROUGH => perform.unhook(),
            _ => {}
        }
    }

    fn clear(&mut self) {
        self.params.clear();
        self.intermediate_len = 0;
        self.ignoring = false;
    }

    fn perform_action<P: Perform>(&mut self, perform: &mut P, action: u8, b: u8) {
        match action {
            action::PRINT => perform.print(b as char),
            action::EXECUTE => perform.execute(b),
            action::IGNORE | action::NONE => {}
            action::COLLECT => {
                if self.intermediate_len < MAX_INTERMEDIATES {
                    self.intermediates[self.intermediate_len] = b;
                    self.intermediate_len += 1;
                } else {
                    self.ignoring = true;
                }
            }
            action::PARAM => {
                if !self.params.push_byte(b) {
                    self.ignoring = true;
                }
            }
            action::CSI_DISPATCH => {
                let inter = self.intermediates[..self.intermediate_len].to_vec();
                perform.csi_dispatch(&self.params, &inter, self.ignoring, b);
            }
            action::ESC_DISPATCH => {
                let inter = self.intermediates[..self.intermediate_len].to_vec();
                perform.esc_dispatch(&inter, self.ignoring, b);
            }
            action::OSC_PUT => {
                if b == b';' {
                    self.osc_ends.push(self.osc_raw.len());
                } else if self.osc_raw.len() < MAX_OSC {
                    self.osc_raw.push(b);
                }
            }
            action::PUT => perform.put(b),
            _ => {}
        }
    }

    fn osc_end<P: Perform>(&mut self, perform: &mut P, bell_terminated: bool) {
        let mut params: Vec<&[u8]> = Vec::with_capacity(self.osc_ends.len() + 1);
        let mut start = 0;
        for &end in &self.osc_ends {
            params.push(&self.osc_raw[start..end]);
            start = end;
        }
        params.push(&self.osc_raw[start..]);
        perform.osc_dispatch(&params, bell_terminated);
    }

    /* ---- Incremental UTF-8 ---------------------------------------------- */

    fn utf8_step<P: Perform>(&mut self, perform: &mut P, b: u8) {
        if self.utf8_needed == 0 {
            self.utf8_needed = match b {
                0xC2..=0xDF => 2,
                0xE0..=0xEF => 3,
                0xF0..=0xF4 => 4,
                // 0x80..=0xC1 and 0xF5.. are never valid lead bytes.
                _ => {
                    perform.print(char::REPLACEMENT_CHARACTER);
                    return;
                }
            };
            self.utf8_buf[0] = b;
            self.utf8_len = 1;
            return;
        }

        if b & 0xC0 != 0x80 {
            // Truncated sequence. Emit a replacement and reprocess this byte as
            // a fresh start, rather than swallowing it — dropping it here would
            // lose a real character after every corrupt one.
            perform.print(char::REPLACEMENT_CHARACTER);
            self.utf8_len = 0;
            self.utf8_needed = 0;
            self.step(perform, b);
            return;
        }

        self.utf8_buf[self.utf8_len] = b;
        self.utf8_len += 1;

        if self.utf8_len == self.utf8_needed {
            match std::str::from_utf8(&self.utf8_buf[..self.utf8_len]) {
                Ok(s) => {
                    if let Some(c) = s.chars().next() {
                        perform.print(c);
                    }
                }
                // Overlong forms and surrogates land here.
                Err(_) => perform.print(char::REPLACEMENT_CHARACTER),
            }
            self.utf8_len = 0;
            self.utf8_needed = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Recorder {
        text: String,
        csi: Vec<(Vec<u16>, u8)>,
        esc: Vec<u8>,
        osc: Vec<Vec<Vec<u8>>>,
        executed: Vec<u8>,
    }

    impl Perform for Recorder {
        fn print(&mut self, c: char) {
            self.text.push(c);
        }
        fn print_ascii(&mut self, bytes: &[u8]) {
            self.text.push_str(std::str::from_utf8(bytes).unwrap());
        }
        fn execute(&mut self, byte: u8) {
            self.executed.push(byte);
        }
        fn csi_dispatch(&mut self, params: &Params, _i: &[u8], _ig: bool, action: u8) {
            self.csi.push((params.iter().collect(), action));
        }
        fn esc_dispatch(&mut self, _i: &[u8], _ig: bool, byte: u8) {
            self.esc.push(byte);
        }
        fn osc_dispatch(&mut self, params: &[&[u8]], _bell: bool) {
            self.osc.push(params.iter().map(|p| p.to_vec()).collect());
        }
    }

    fn run(input: &[u8]) -> Recorder {
        let mut p = Parser::new();
        let mut r = Recorder::default();
        p.advance(&mut r, input);
        r
    }

    #[test]
    fn plain_ascii() {
        assert_eq!(run(b"hello world").text, "hello world");
    }

    #[test]
    fn ascii_run_stops_at_controls_and_high_bytes() {
        assert_eq!(ascii_run_len(b"abc\ndef"), 3);
        assert_eq!(ascii_run_len(b"\nabc"), 0);
        assert_eq!(ascii_run_len(b"0123456789abcdef\x1b"), 16);
        assert_eq!(ascii_run_len("caf\u{e9}".as_bytes()), 3);
        assert_eq!(ascii_run_len(b"exactly8"), 8);
        assert_eq!(ascii_run_len(b""), 0);
        assert_eq!(ascii_run_len(b"\x7f"), 0);
    }

    #[test]
    fn swar_and_scalar_scans_agree() {
        // The word-at-a-time path must never disagree with the obvious loop.
        for len in 0..40usize {
            for bad in 0..len {
                let mut v = vec![b'x'; len];
                v[bad] = b'\n';
                assert_eq!(ascii_run_len(&v), bad, "len={len} bad={bad}");
            }
        }
    }

    #[test]
    fn csi_parameters() {
        let r = run(b"\x1b[1;31m");
        assert_eq!(r.csi, vec![(vec![1, 31], b'm')]);
    }

    #[test]
    fn csi_colon_subparams_are_kept() {
        // The classic table would discard this; truecolour SGR needs it.
        let r = run(b"\x1b[38:2::255:100:0m");
        assert_eq!(r.csi, vec![(vec![38, 2, 0, 255, 100, 0], b'm')]);
    }

    #[test]
    fn csi_empty_params_default_to_zero() {
        let r = run(b"\x1b[;5H");
        assert_eq!(r.csi, vec![(vec![0, 5], b'H')]);
    }

    #[test]
    fn esc_dispatch() {
        assert_eq!(run(b"\x1bM").esc, vec![b'M']);
    }

    #[test]
    fn execute_control_bytes() {
        let r = run(b"a\nb\r");
        assert_eq!(r.text, "ab");
        assert_eq!(r.executed, vec![b'\n', b'\r']);
    }

    #[test]
    fn osc_title_bell_terminated() {
        let r = run(b"\x1b]0;my title\x07");
        assert_eq!(r.osc.len(), 1);
        assert_eq!(r.osc[0][0], b"0");
        assert_eq!(r.osc[0][1], b"my title");
    }

    #[test]
    fn osc_string_terminator() {
        let r = run(b"\x1b]2;abc\x1b\\");
        assert_eq!(r.osc.len(), 1);
        assert_eq!(r.osc[0][1], b"abc");
    }

    #[test]
    fn utf8_is_decoded() {
        assert_eq!(run("héllo — ✓".as_bytes()).text, "héllo — ✓");
    }

    #[test]
    fn utf8_split_across_writes_is_not_corrupted() {
        // The whole reason the decoder is incremental: a PTY read can end
        // mid-character.
        let bytes = "é✓".as_bytes().to_vec();
        for split in 1..bytes.len() {
            let mut p = Parser::new();
            let mut r = Recorder::default();
            p.advance(&mut r, &bytes[..split]);
            p.advance(&mut r, &bytes[split..]);
            assert_eq!(r.text, "é✓", "split at {split}");
        }
    }

    #[test]
    fn invalid_utf8_yields_replacement_and_keeps_going() {
        let r = run(b"a\xFFb");
        assert_eq!(r.text, "a\u{FFFD}b");
    }

    #[test]
    fn c1_bytes_are_not_controls_in_utf8_mode() {
        // 0x9C is a UTF-8 continuation byte here, not a String Terminator.
        let r = run("œ".as_bytes());
        assert_eq!(r.text, "œ");
    }

    #[test]
    fn can_and_sub_abort_a_sequence() {
        let r = run(b"\x1b[12\x18x");
        assert!(r.csi.is_empty());
        assert_eq!(r.text, "x");
    }

    #[test]
    fn in_string_tracks_osc() {
        let mut p = Parser::new();
        let mut r = Recorder::default();
        assert!(!p.in_string());
        p.advance(&mut r, b"\x1b]0;t");
        assert!(p.in_string());
        p.advance(&mut r, b"\x07");
        assert!(!p.in_string());
    }
}
