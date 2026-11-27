//! Key → terminal bytes, natively (xterm encoding).
//!
//! The Rust twin of `src/lib/keys.ts`. While a GPU-drawn terminal has focus,
//! `gpu_term` encodes keys here and writes them straight to the PTY, skipping
//! WebKit, the page and IPC: measured at 1.5 ms of every keystroke
//! (CLAUDE.md §18.24). The two encoders must agree; the tests below are the TS
//! suite's cases (`scripts/test-keys.ts`) ported one for one.
//!
//! `encode` returns `None` for anything the page must still see, and those
//! keys reach WebKit untouched: every app chord, dead keys and IME input, and
//! anything this table does not know. Never swallow a key you do not handle.

/// GDK keysyms (gdkkeysyms.h). Plain numbers so this module has no GTK
/// dependency and its tests run on every platform.
pub mod sym {
    pub const BACKSPACE: u32 = 0xff08;
    pub const TAB: u32 = 0xff09;
    pub const ISO_LEFT_TAB: u32 = 0xfe20;
    pub const RETURN: u32 = 0xff0d;
    pub const KP_ENTER: u32 = 0xff8d;
    pub const ESCAPE: u32 = 0xff1b;
    pub const HOME: u32 = 0xff50;
    pub const LEFT: u32 = 0xff51;
    pub const UP: u32 = 0xff52;
    pub const RIGHT: u32 = 0xff53;
    pub const DOWN: u32 = 0xff54;
    pub const PAGE_UP: u32 = 0xff55;
    pub const PAGE_DOWN: u32 = 0xff56;
    pub const END: u32 = 0xff57;
    pub const INSERT: u32 = 0xff63;
    pub const DELETE: u32 = 0xffff;
    pub const KP_HOME: u32 = 0xff95;
    pub const KP_LEFT: u32 = 0xff96;
    pub const KP_UP: u32 = 0xff97;
    pub const KP_RIGHT: u32 = 0xff98;
    pub const KP_DOWN: u32 = 0xff99;
    pub const KP_PAGE_UP: u32 = 0xff9a;
    pub const KP_PAGE_DOWN: u32 = 0xff9b;
    pub const KP_END: u32 = 0xff9c;
    pub const KP_INSERT: u32 = 0xff9e;
    pub const KP_DELETE: u32 = 0xff9f;
    pub const F1: u32 = 0xffbe;
    pub const F12: u32 = 0xffc9;
}

/// One key press, reduced to what the encoder needs.
#[derive(Clone, Copy, Debug, Default)]
pub struct Key {
    pub keyval: u32,
    /// The character the key produces with the current layout and modifiers
    /// (Shift and AltGr already applied), if it produces one.
    pub text: Option<char>,
    /// The Latin letter printed on the same physical key, for Ctrl on a
    /// non-Latin layout: Ctrl+C must still interrupt with Cyrillic active.
    pub latin: Option<char>,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    /// Super, Meta or Hyper: the OS's shortcuts, never the terminal's.
    pub super_: bool,
}

/// Keys the page claims (AppShell, SessionTabs, PaneTree, TerminalSurface).
/// These are not encoded here even where the encoder has bytes for them.
fn belongs_to_app(k: &Key) -> bool {
    use sym::*;
    let v = k.keyval;
    let page = matches!(v, PAGE_UP | PAGE_DOWN | KP_PAGE_UP | KP_PAGE_DOWN);
    let insert = matches!(v, INSERT | KP_INSERT);
    let horiz = matches!(v, LEFT | RIGHT | KP_LEFT | KP_RIGHT);
    // Every app chord is Ctrl+Shift+something off macOS (`isAppChord`), and
    // so are a few shell keys (Ctrl+_ is undo). The page encodes those itself.
    (k.ctrl && k.shift)
        // Ctrl+PageUp/PageDown cycles tabs; Shift+PageUp/PageDown scrolls.
        || (page && (k.ctrl || k.shift))
        // Shift+Insert pastes, Ctrl+Insert copies.
        || (insert && (k.ctrl || k.shift))
        // Ctrl+Alt+Left/Right moves focus between panes.
        || (horiz && k.ctrl && k.alt)
        // Alt+Shift+= and Alt+Shift+- split the pane.
        || (k.alt && k.shift && matches!(k.text, Some('=' | '+' | '-' | '_')))
        // Ctrl+1..9 switches tabs; Ctrl+= / Ctrl+- / Ctrl+0 zoom the font.
        || (k.ctrl && matches!(k.text, Some('0'..='9' | '=' | '+' | '-')))
}

/// xterm's modifier parameter: 1 + Shift(1) + Alt(2) + Ctrl(4).
fn mod_param(k: &Key) -> u8 {
    1 + u8::from(k.shift) + 2 * u8::from(k.alt) + 4 * u8::from(k.ctrl)
}

/// The bytes for a key, or `None` to let the page handle it.
pub fn encode(k: &Key, app_cursor: bool) -> Option<Vec<u8>> {
    use sym::*;
    if k.super_ || belongs_to_app(k) {
        return None;
    }
    let m = mod_param(k);
    let esc = |s: &[u8]| -> Vec<u8> {
        let mut v = Vec::with_capacity(s.len() + 1);
        if k.alt {
            v.push(0x1b);
        }
        v.extend_from_slice(s);
        v
    };

    let cursor = match k.keyval {
        UP | KP_UP => Some(b'A'),
        DOWN | KP_DOWN => Some(b'B'),
        RIGHT | KP_RIGHT => Some(b'C'),
        LEFT | KP_LEFT => Some(b'D'),
        HOME | KP_HOME => Some(b'H'),
        END | KP_END => Some(b'F'),
        _ => None,
    };
    if let Some(c) = cursor {
        // Modified cursor keys always use the CSI form, whatever DECCKM says.
        if m > 1 {
            return Some(format!("\x1b[1;{m}{}", c as char).into_bytes());
        }
        return Some(vec![0x1b, if app_cursor { b'O' } else { b'[' }, c]);
    }

    if (sym::F1..=sym::F12).contains(&k.keyval) {
        let n = k.keyval - sym::F1 + 1;
        if n <= 4 {
            let c = b"PQRS"[(n - 1) as usize] as char;
            return Some(if m > 1 { format!("\x1b[1;{m}{c}") } else { format!("\x1bO{c}") }.into_bytes());
        }
        let code = [15, 17, 18, 19, 20, 21, 23, 24][(n - 5) as usize];
        return Some(tilde(code, m));
    }
    let t = match k.keyval {
        INSERT | KP_INSERT => Some(2),
        DELETE | KP_DELETE => Some(3),
        PAGE_UP | KP_PAGE_UP => Some(5),
        PAGE_DOWN | KP_PAGE_DOWN => Some(6),
        _ => None,
    };
    if let Some(code) = t {
        return Some(tilde(code, m));
    }

    match k.keyval {
        RETURN | KP_ENTER => return Some(esc(b"\r")),
        ESCAPE => return Some(esc(b"\x1b")),
        // GTK reports Shift+Tab as ISO_Left_Tab.
        ISO_LEFT_TAB => return Some(b"\x1b[Z".to_vec()),
        TAB => return Some(if k.shift { b"\x1b[Z".to_vec() } else { esc(b"\t") }),
        // DEL, as every Linux terminal sends; Ctrl+Backspace sends BS.
        BACKSPACE => return Some(if k.ctrl { vec![0x08] } else { esc(b"\x7f") }),
        _ => {}
    }

    // Shift, Control, dead keys, media keys: no character, not ours.
    let ch = k.text.filter(|c| !c.is_control())?;

    if k.ctrl {
        // Ctrl+A..Z → 0x01..0x1A. The physical key decides on non-Latin
        // layouts.
        let lower = ch.to_ascii_lowercase();
        let letter = if lower.is_ascii_lowercase() { Some(lower) } else { k.latin.map(|c| c.to_ascii_lowercase()) };
        let byte = match letter {
            Some(c @ 'a'..='z') => c as u8 - b'a' + 1,
            _ => match ch {
                '@' | ' ' => 0x00,
                '[' => 0x1b,
                '\\' => 0x1c,
                ']' => 0x1d,
                '^' => 0x1e,
                '_' | '/' => 0x1f,
                '?' => 0x7f,
                _ => return None,
            },
        };
        return Some(if k.alt { vec![0x1b, byte] } else { vec![byte] });
    }

    // Alt+key sends ESC first ("meta sends escape"). On X11, AltGr is not Alt:
    // its character arrives already composed in `text`.
    let mut buf = [0u8; 4];
    Some(esc(ch.encode_utf8(&mut buf).as_bytes()))
}

fn tilde(code: u8, m: u8) -> Vec<u8> {
    if m > 1 { format!("\x1b[{code};{m}~") } else { format!("\x1b[{code}~") }.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::sym::*;
    use super::*;

    fn key(keyval: u32) -> Key {
        Key { keyval, ..Key::default() }
    }
    fn ch(c: char) -> Key {
        Key { keyval: c as u32, text: Some(c), ..Key::default() }
    }
    fn s(k: Key) -> Option<String> {
        encode(&k, false).map(|b| String::from_utf8(b).unwrap())
    }

    #[test]
    fn arrows_honour_decckm() {
        assert_eq!(encode(&key(UP), false).unwrap(), b"\x1b[A");
        assert_eq!(encode(&key(UP), true).unwrap(), b"\x1bOA");
        assert_eq!(encode(&key(HOME), true).unwrap(), b"\x1bOH");
    }

    #[test]
    fn modified_cursor_keys_use_the_xterm_parameter_form_in_either_mode() {
        let ctrl_left = Key { ctrl: true, ..key(LEFT) };
        assert_eq!(encode(&ctrl_left, false).unwrap(), b"\x1b[1;5D");
        assert_eq!(encode(&ctrl_left, true).unwrap(), b"\x1b[1;5D");
        assert_eq!(s(Key { alt: true, ..key(RIGHT) }).unwrap(), "\x1b[1;3C");
        assert_eq!(s(Key { shift: true, ..key(END) }).unwrap(), "\x1b[1;2F");
    }

    #[test]
    fn function_and_editing_keys() {
        assert_eq!(s(key(F1)).unwrap(), "\x1bOP");
        assert_eq!(s(Key { ctrl: true, ..key(F1) }).unwrap(), "\x1b[1;5P");
        assert_eq!(s(key(F1 + 4)).unwrap(), "\x1b[15~");
        assert_eq!(s(key(F12)).unwrap(), "\x1b[24~");
        assert_eq!(s(Key { ctrl: true, ..key(DELETE) }).unwrap(), "\x1b[3;5~");
        assert_eq!(s(Key { shift: true, ..key(ISO_LEFT_TAB) }).unwrap(), "\x1b[Z");
        assert_eq!(s(key(BACKSPACE)).unwrap(), "\x7f");
        assert_eq!(s(Key { ctrl: true, ..key(BACKSPACE) }).unwrap(), "\x08");
        assert_eq!(s(Key { alt: true, ..key(BACKSPACE) }).unwrap(), "\x1b\x7f");
        assert_eq!(s(key(RETURN)).unwrap(), "\r");
        assert_eq!(s(key(PAGE_UP)).unwrap(), "\x1b[5~");
    }

    #[test]
    fn ctrl_letters_reach_the_shell_including_the_ones_the_app_used_to_steal() {
        for (c, byte) in [('w', 0x17), ('k', 0x0b), ('b', 0x02), ('i', 0x09), ('c', 0x03)] {
            assert_eq!(encode(&Key { ctrl: true, ..ch(c) }, false).unwrap(), vec![byte], "Ctrl+{c}");
        }
        assert_eq!(encode(&Key { ctrl: true, alt: true, ..ch('b') }, false).unwrap(), vec![0x1b, 0x02]);
        assert_eq!(encode(&Key { ctrl: true, ..ch('[') }, false).unwrap(), vec![0x1b]);
    }

    #[test]
    fn ctrl_c_interrupts_on_a_non_latin_layout() {
        // Russian layout: the C key produces "с" (Cyrillic es).
        let k = Key { ctrl: true, latin: Some('c'), ..ch('с') };
        assert_eq!(encode(&k, false).unwrap(), vec![0x03]);
    }

    #[test]
    fn text_is_typed_as_utf8_and_alt_prefixes_escape() {
        assert_eq!(s(ch('a')).unwrap(), "a");
        assert_eq!(s(ch('é')).unwrap(), "é");
        // AltGr+Q on a German layout: GTK hands over "@" with no Ctrl or Alt.
        assert_eq!(s(ch('@')).unwrap(), "@");
        assert_eq!(s(Key { alt: true, ..ch('f') }).unwrap(), "\x1bf");
    }

    #[test]
    fn keys_that_are_not_ours_are_left_alone() {
        assert_eq!(s(key(0xffe1)), None, "Shift alone");
        assert_eq!(s(Key { super_: true, ..ch('a') }), None);
        assert_eq!(s(key(0xfe51)), None, "a dead key produces no character");
        assert_eq!(s(Key { ctrl: true, ..ch('.') }), None, "no control code");
    }

    #[test]
    fn app_chords_go_to_the_page() {
        let cases = [
            Key { ctrl: true, shift: true, ..ch('K') },   // palette
            Key { ctrl: true, shift: true, ..ch('C') },   // copy
            Key { ctrl: true, shift: true, ..ch('_') },   // undo: the page encodes it
            Key { ctrl: true, ..key(PAGE_DOWN) },         // next tab
            Key { shift: true, ..key(PAGE_UP) },          // scrollback
            Key { shift: true, ..key(INSERT) },           // paste
            Key { ctrl: true, ..key(INSERT) },            // copy
            Key { ctrl: true, alt: true, ..key(RIGHT) },  // focus next pane
            Key { alt: true, shift: true, ..ch('+') },    // split
            Key { alt: true, shift: true, ..ch('_') },    // split
            Key { ctrl: true, ..ch('3') },                // tab 3
            Key { ctrl: true, ..ch('=') },                // zoom in
            Key { ctrl: true, ..ch('-') },                // zoom out
            Key { ctrl: true, ..ch('0') },                // zoom reset
        ];
        for k in cases {
            assert_eq!(encode(&k, false), None, "{k:?} must reach the page");
        }
        // ...while their unmodified neighbours are the shell's.
        assert!(encode(&key(PAGE_UP), false).is_some());
        assert!(encode(&Key { ctrl: true, ..key(RIGHT) }, false).is_some());
        assert!(encode(&ch('3'), false).is_some());
    }
}
