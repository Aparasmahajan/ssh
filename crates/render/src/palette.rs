//! Colour resolution: the grid's tagged `u32` colours to RGBA.
//!
//! Same values as the webview path (`crates/app/src/session.rs`, `ANSI`), so
//! a pane looks the same whichever path drew it.

use opspilot_core_term::style::color;

/// `0xRRGGBB` → the packed RGBA the shader unpacks with `unpack4x8unorm`
/// (red in the low byte).
pub const fn rgba(rgb: u32, alpha: u8) -> u32 {
    let (r, g, b) = ((rgb >> 16) & 0xFF, (rgb >> 8) & 0xFF, rgb & 0xFF);
    r | (g << 8) | (b << 16) | ((alpha as u32) << 24)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    /// All `0xRRGGBB`.
    pub fg: u32,
    pub bg: u32,
    pub cursor: u32,
    pub selection: u32,
    pub selection_alpha: u8,
    pub ansi: [u32; 16],
}

impl Default for Palette {
    fn default() -> Self {
        Palette {
            fg: 0xE8ECF2,     // --text-hi
            bg: 0x14171C,     // --chassis-900
            cursor: 0xF2A93B, // --amber-500
            selection: 0xF2A93B,
            selection_alpha: 82, // 0.32, the canvas path's SELECTION_ALPHA
            ansi: [
                0x1B1F26, 0xE5484D, 0x4FD1C5, 0xF2A93B, 0x6B8AFD, 0xA78BFA, 0x4FD1C5, 0x9AA6B8, //
                0x3A4252, 0xFF6369, 0x6EE7DB, 0xFFC46B, 0x8FA6FF, 0xC4B5FD, 0x7FE3DA, 0xE8ECF2,
            ],
        }
    }
}

impl Palette {
    /// A grid colour to `0xRRGGBB`. O(1).
    pub fn resolve(&self, c: u32, is_bg: bool) -> u32 {
        if c & color::TAG_DEFAULT != 0 {
            return if is_bg { self.bg } else { self.fg };
        }
        if c & color::TAG_INDEXED != 0 {
            let i = (c & 0xFF) as usize;
            if i < 16 {
                return self.ansi[i];
            }
            if i < 232 {
                // 6×6×6 cube.
                let j = i - 16;
                let f = |v: usize| if v == 0 { 0 } else { (55 + v * 40) as u32 };
                return (f(j / 36) << 16) | (f((j / 6) % 6) << 8) | f(j % 6);
            }
            let v = (8 + (i - 232) * 10) as u32;
            return (v << 16) | (v << 8) | v;
        }
        c & 0xFF_FFFF
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_every_kind_of_colour() {
        let p = Palette::default();
        assert_eq!(p.resolve(color::DEFAULT_FG, false), p.fg);
        assert_eq!(p.resolve(color::DEFAULT_BG, true), p.bg);
        assert_eq!(p.resolve(color::indexed(1), false), 0xE5484D);
        assert_eq!(p.resolve(color::indexed(16), false), 0x000000);
        assert_eq!(p.resolve(color::indexed(231), false), 0xFFFFFF);
        assert_eq!(p.resolve(color::indexed(232), false), 0x080808);
        assert_eq!(p.resolve(color::rgb(1, 2, 3), false), 0x010203);
    }

    #[test]
    fn rgba_puts_red_in_the_low_byte() {
        assert_eq!(rgba(0x112233, 0x44), 0x44332211);
    }
}
