//! The glyph atlas: one 2048×2048 R8 texture (§5.7), shelf-packed.
//!
//! The CPU side keeps no copy of the texture, only the packing state and a
//! queue of bitmaps waiting to be uploaded: a 4 MiB mirror would be a tenth of
//! §4's per-terminal RSS budget for nothing. When the atlas fills, it is
//! cleared and refilled from what is on screen (§5.7's "repack on pressure").
//! At the default size it holds about 30,000 glyphs, so that is rare.

use std::collections::HashMap;

use crate::font::{Bitmap, Face, Fonts};

pub const ATLAS_SIZE: u32 = 2048;

/// Where a glyph lives in the atlas and where it sits in its cell.
/// `w == 0` is a glyph with no ink (a space), which draws nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Slot {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    pub left: i16,
    pub top: i16,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    ch: u32,
    face: Face,
    wide: bool,
}

struct Shelf {
    y: u32,
    h: u32,
    x: u32,
}

pub struct Upload {
    pub slot: Slot,
    pub data: Vec<u8>,
}

pub struct Glyphs {
    pub fonts: Fonts,
    shelves: Vec<Shelf>,
    next_y: u32,
    map: HashMap<Key, Slot>,
    /// Printable ASCII in the regular face, indexed directly: the bulk of every
    /// frame, so it skips the hash.
    ascii: [Option<Slot>; 95],
    pending: Vec<Upload>,
    /// Bumped when the atlas is cleared. Any instance built against an older
    /// epoch points at slots that now hold other glyphs.
    pub epoch: u32,
}

impl Glyphs {
    pub fn new(fonts: Fonts) -> Glyphs {
        let mut g = Glyphs {
            fonts,
            shelves: Vec::new(),
            next_y: 0,
            map: HashMap::new(),
            ascii: [None; 95],
            pending: Vec::new(),
            epoch: 0,
        };
        // §5.7: pre-rasterize ASCII at startup.
        for c in ' '..='~' {
            g.slot(c as u32, Face::Regular, false);
        }
        g
    }

    /// The atlas slot for a character, rasterizing it on first use.
    pub fn slot(&mut self, ch: u32, face: Face, wide: bool) -> Slot {
        let ascii = face == Face::Regular && !wide && (0x20..0x7F).contains(&ch);
        if ascii {
            if let Some(s) = self.ascii[(ch - 0x20) as usize] {
                return s;
            }
        }
        let key = Key { ch, face, wide };
        if let Some(&s) = self.map.get(&key) {
            return s;
        }
        let c = char::from_u32(ch).unwrap_or('\u{FFFD}');
        let bitmap = self.fonts.rasterize(c, face, if wide { 2 } else { 1 });
        let slot = match self.place(&bitmap) {
            Some(s) => s,
            None => {
                self.clear();
                // A glyph larger than the whole atlas is not drawn at all.
                self.place(&bitmap).unwrap_or_default()
            }
        };
        if slot.w > 0 {
            self.pending.push(Upload { slot, data: bitmap.data });
        }
        if ascii {
            self.ascii[(ch - 0x20) as usize] = Some(slot);
        } else {
            self.map.insert(key, slot);
        }
        slot
    }

    /// Bitmaps waiting for the GPU. The renderer drains this before drawing.
    pub fn take_uploads(&mut self) -> Vec<Upload> {
        std::mem::take(&mut self.pending)
    }

    pub fn has_uploads(&self) -> bool {
        !self.pending.is_empty()
    }

    fn clear(&mut self) {
        self.shelves.clear();
        self.next_y = 0;
        self.map.clear();
        self.ascii = [None; 95];
        self.pending.clear();
        self.epoch = self.epoch.wrapping_add(1);
    }

    /// First fit on a shelf no more than 25% taller than the glyph, or a new
    /// shelf. O(shelves), and there are at most a few hundred.
    fn place(&mut self, b: &Bitmap) -> Option<Slot> {
        let mk = |x: u32, y: u32| Slot {
            x: x as u16,
            y: y as u16,
            w: b.width as u16,
            h: b.height as u16,
            left: b.left as i16,
            top: b.top as i16,
        };
        if b.is_empty() {
            return Some(Slot { left: b.left as i16, top: b.top as i16, ..Slot::default() });
        }
        if b.width > ATLAS_SIZE || b.height > ATLAS_SIZE {
            return None;
        }
        for s in &mut self.shelves {
            if s.h >= b.height && s.h <= b.height + b.height / 4 + 1 && s.x + b.width <= ATLAS_SIZE {
                let slot = mk(s.x, s.y);
                s.x += b.width;
                return Some(slot);
            }
        }
        if self.next_y + b.height > ATLAS_SIZE {
            return None;
        }
        let y = self.next_y;
        self.next_y += b.height;
        self.shelves.push(Shelf { y, h: b.height, x: b.width });
        Some(mk(0, y))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_ready_before_the_first_frame() {
        let mut g = Glyphs::new(Fonts::new(12.5, 17));
        let n = g.take_uploads().len();
        assert_eq!(n, 94, "every printable ASCII glyph but the space queued for upload");
        let a = g.slot('a' as u32, Face::Regular, false);
        assert!(a.w > 0 && a.h > 0);
        assert!(!g.has_uploads(), "a cached glyph is not uploaded twice");
    }

    #[test]
    fn slots_never_overlap() {
        let mut g = Glyphs::new(Fonts::new(12.5, 17));
        for face in [Face::Bold, Face::Italic, Face::BoldItalic] {
            for c in '!'..='~' {
                g.slot(c as u32, face, false);
            }
        }
        let slots: Vec<Slot> = g.take_uploads().into_iter().map(|u| u.slot).collect();
        for (i, a) in slots.iter().enumerate() {
            assert!(a.x as u32 + a.w as u32 <= ATLAS_SIZE && a.y as u32 + a.h as u32 <= ATLAS_SIZE);
            for b in &slots[i + 1..] {
                let apart = a.x + a.w <= b.x || b.x + b.w <= a.x || a.y + a.h <= b.y || b.y + b.h <= a.y;
                assert!(apart, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn a_full_atlas_clears_and_bumps_the_epoch() {
        // A huge size fills 2048² after a few dozen glyphs.
        let mut g = Glyphs::new(Fonts::new(400.0, 520));
        let before = g.epoch;
        for c in 'A'..='z' {
            g.slot(c as u32, Face::Bold, false);
        }
        assert_ne!(g.epoch, before, "the atlas should have filled and been cleared");
        // What survives the clear is still valid.
        let z = g.slot('z' as u32, Face::Bold, false);
        assert!(z.w > 0);
    }
}
