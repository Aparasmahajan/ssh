//! Style interning.
//!
//! CLAUDE.md §5.1: "Most screens use < 256 distinct (fg, bg, flags)
//! combinations. Interning them cuts the per-cell footprint from 16 bytes to 6."
//!
//! The whole point is that the grid stores a `u16` per cell instead of a 10-byte
//! `Style`. A 200x50 viewport is then 60 KB and fits in L2 (§5.1), and the GPU
//! upload path moves ~2.6x fewer bytes.

use std::collections::HashMap;

/// Bit flags for a cell's rendition. `u16` so `Style` stays 10 bytes and hashes
/// as three machine words.
pub mod flags {
    pub const BOLD: u16 = 1 << 0;
    pub const DIM: u16 = 1 << 1;
    pub const ITALIC: u16 = 1 << 2;
    pub const UNDERLINE: u16 = 1 << 3;
    pub const BLINK: u16 = 1 << 4;
    pub const REVERSE: u16 = 1 << 5;
    pub const HIDDEN: u16 = 1 << 6;
    pub const STRIKE: u16 = 1 << 7;
    /// Rendered with the prediction treatment (UI_BUILD_GUIDE §5.5 "underline
    /// unconfirmed predictions"). Lives here so a predicted cell is a style
    /// variant rather than a parallel plane the renderer has to consult.
    pub const PREDICTED: u16 = 1 << 8;
}

/// A resolved cell rendition. Colours are packed 0x00RRGGBB, or an indexed
/// colour with the high byte set — see [`Color`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Style {
    pub fg: u32,
    pub bg: u32,
    pub flags: u16,
}

/// Colour helpers. The high byte distinguishes "default", "indexed" and "rgb"
/// so the whole colour fits one `u32` and needs no enum discriminant in the
/// hot plane.
pub mod color {
    pub const TAG_DEFAULT: u32 = 0x0100_0000;
    pub const TAG_INDEXED: u32 = 0x0200_0000;

    pub const DEFAULT_FG: u32 = TAG_DEFAULT;
    pub const DEFAULT_BG: u32 = TAG_DEFAULT | 1;

    #[inline]
    pub const fn indexed(i: u8) -> u32 {
        TAG_INDEXED | i as u32
    }

    #[inline]
    pub const fn rgb(r: u8, g: u8, b: u8) -> u32 {
        ((r as u32) << 16) | ((g as u32) << 8) | b as u32
    }

    #[inline]
    pub const fn is_rgb(c: u32) -> bool {
        c & 0xFF00_0000 == 0
    }
}

impl Default for Style {
    fn default() -> Self {
        Style {
            fg: color::DEFAULT_FG,
            bg: color::DEFAULT_BG,
            flags: 0,
        }
    }
}

/// Index into a [`StyleTable`]. Slot 0 is always the default style, so a zeroed
/// grid is a blank screen with no lookup required.
pub type StyleId = u16;

pub const DEFAULT_STYLE: StyleId = 0;

/// Interning table, one per screen.
#[derive(Debug)]
pub struct StyleTable {
    entries: Vec<Style>,
    lookup: HashMap<Style, StyleId>,
}

impl Default for StyleTable {
    fn default() -> Self {
        Self::new()
    }
}

impl StyleTable {
    pub fn new() -> Self {
        let default = Style::default();
        let mut lookup = HashMap::with_capacity(64);
        lookup.insert(default, DEFAULT_STYLE);
        StyleTable {
            entries: vec![default],
            lookup,
        }
    }

    /// Intern a style, returning its id.
    ///
    /// Saturates rather than growing without bound: a program emitting a fresh
    /// 24-bit colour per cell (a truecolour image dumped to the terminal) would
    /// otherwise grow this table by one entry per cell and exhaust `u16`.
    /// Past the cap, new styles collapse onto the last slot — visually wrong for
    /// a pathological case, but bounded, and it cannot panic on a live session.
    pub fn intern(&mut self, style: Style) -> StyleId {
        if let Some(&id) = self.lookup.get(&style) {
            return id;
        }
        if self.entries.len() >= StyleId::MAX as usize {
            return (self.entries.len() - 1) as StyleId;
        }
        let id = self.entries.len() as StyleId;
        self.entries.push(style);
        self.lookup.insert(style, id);
        id
    }

    #[inline]
    pub fn get(&self, id: StyleId) -> Style {
        self.entries.get(id as usize).copied().unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        false // slot 0 always exists
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_style_is_slot_zero() {
        let t = StyleTable::new();
        assert_eq!(t.get(DEFAULT_STYLE), Style::default());
    }

    #[test]
    fn interning_is_stable_and_deduplicates() {
        let mut t = StyleTable::new();
        let s = Style {
            fg: color::rgb(0xF2, 0xA9, 0x3B),
            bg: color::DEFAULT_BG,
            flags: flags::BOLD,
        };
        let a = t.intern(s);
        let b = t.intern(s);
        assert_eq!(a, b);
        assert_eq!(t.len(), 2);
        assert_eq!(t.get(a), s);
    }

    #[test]
    fn interning_saturates_instead_of_overflowing() {
        let mut t = StyleTable::new();
        // One unique style per iteration, past the u16 ceiling.
        for i in 0..(u16::MAX as u32 + 100) {
            t.intern(Style {
                fg: i,
                bg: color::DEFAULT_BG,
                flags: 0,
            });
        }
        assert!(t.len() <= u16::MAX as usize);
    }
}
