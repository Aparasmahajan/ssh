//! Character display width.
//!
//! CLAUDE.md §5.1: "Wide (CJK/emoji) cells occupy two slots; the second carries
//! `WIDE_SPACER`." Nothing computed that width until now, so every CJK character
//! and every emoji occupied one cell and the rest of the line drifted left by one
//! column per character — the classic misaligned-`ls` bug.
//!
//! ## Why a table and not a crate
//!
//! `unicode-width` is the obvious dependency, and §16 asks for a note on any
//! new one. It is ~40 KB of tables for full Unicode coverage. What a terminal
//! actually needs is the East Asian Wide/Fullwidth ranges plus emoji, which is a
//! few dozen ranges and a binary search — and it keeps `core-term` at zero
//! runtime dependencies, which is what makes it trivially portable and keeps it
//! out of the cold-start budget (§14.5).
//!
//! The tables follow Unicode 15's East Asian Width property (`W` and `F`) and
//! the `Emoji_Presentation` set. They are not exhaustive over every historic
//! script, and that is a deliberate trade: the ranges here cover what people
//! actually put in terminals.

/// Ranges whose characters occupy two cells. Sorted, non-overlapping.
const WIDE: &[(u32, u32)] = &[
    (0x1100, 0x115F), // Hangul Jamo initial consonants
    // Emoji_Presentation characters scattered through the BMP symbol blocks.
    // These are listed individually rather than as whole blocks because their
    // neighbours are NOT wide: U+2705 ✅ is emoji and takes two cells, while
    // U+2713 ✓ two codepoints away is a text check mark and takes one. Widening
    // the whole block would misalign every TUI that uses ✓, ✗ or ⚠.
    (0x231A, 0x231B),   // watch, hourglass
    (0x23E9, 0x23EC),   // fast-forward family
    (0x23F0, 0x23F0),   // alarm clock
    (0x23F3, 0x23F3),   // hourglass flowing
    (0x25FD, 0x25FE),   // small squares (emoji presentation)
    (0x2614, 0x2615),   // umbrella, hot beverage
    (0x2648, 0x2653),   // zodiac
    (0x267F, 0x267F),   // wheelchair
    (0x2693, 0x2693),   // anchor
    (0x26A1, 0x26A1),   // high voltage
    (0x26AA, 0x26AB),   // medium circles
    (0x26BD, 0x26BE),   // soccer, baseball
    (0x26C4, 0x26C5),   // snowman, sun behind cloud
    (0x26CE, 0x26CE),   // ophiuchus
    (0x26D4, 0x26D4),   // no entry
    (0x26EA, 0x26EA),   // church
    (0x26F2, 0x26F3),   // fountain, golf
    (0x26F5, 0x26F5),   // sailboat
    (0x26FA, 0x26FA),   // tent
    (0x26FD, 0x26FD),   // fuel pump
    (0x2705, 0x2705),   // white heavy check mark  (NOT U+2713 ✓)
    (0x270A, 0x270B),   // raised fist, raised hand
    (0x2728, 0x2728),   // sparkles
    (0x274C, 0x274C),   // cross mark             (NOT U+2717 ✗)
    (0x274E, 0x274E),   // negative squared cross
    (0x2753, 0x2755),   // question / exclamation ornaments
    (0x2757, 0x2757),   // heavy exclamation
    (0x2795, 0x2797),   // heavy plus/minus/divide
    (0x27B0, 0x27B0),   // curly loop
    (0x27BF, 0x27BF),   // double curly loop
    (0x2B1B, 0x2B1C),   // large squares
    (0x2B50, 0x2B50),   // white medium star
    (0x2B55, 0x2B55),   // heavy large circle
    (0x2E80, 0x303E),   // CJK radicals, Kangxi, CJK symbols
    (0x3041, 0x33FF),   // Hiragana, Katakana, Bopomofo, Hangul compat, CJK compat
    (0x3400, 0x4DBF),   // CJK unified ideographs extension A
    (0x4E00, 0x9FFF),   // CJK unified ideographs
    (0xA000, 0xA4CF),   // Yi
    (0xA960, 0xA97F),   // Hangul Jamo extended-A
    (0xAC00, 0xD7A3),   // Hangul syllables
    (0xF900, 0xFAFF),   // CJK compatibility ideographs
    (0xFE10, 0xFE19),   // Vertical forms
    (0xFE30, 0xFE6F),   // CJK compatibility forms, small form variants
    (0xFF00, 0xFF60),   // Fullwidth forms
    (0xFFE0, 0xFFE6),   // Fullwidth signs
    (0x1F004, 0x1F004), // Mahjong red dragon
    (0x1F0CF, 0x1F0CF), // Playing card black joker
    (0x1F18E, 0x1F18E), // Negative squared AB
    (0x1F191, 0x1F19A), // Squared CL..VS
    (0x1F200, 0x1F320), // Enclosed ideographic supplement, weather
    (0x1F32D, 0x1F335), // Food
    (0x1F337, 0x1F37C),
    (0x1F37E, 0x1F393),
    (0x1F3A0, 0x1F3CA),
    (0x1F3CF, 0x1F3D3),
    (0x1F3E0, 0x1F3F0),
    (0x1F3F4, 0x1F3F4),
    (0x1F3F8, 0x1F43E),
    (0x1F440, 0x1F440),
    (0x1F442, 0x1F4FC),
    (0x1F4FF, 0x1F53D),
    (0x1F54B, 0x1F54E),
    (0x1F550, 0x1F567),
    (0x1F57A, 0x1F57A),
    (0x1F595, 0x1F596),
    (0x1F5A4, 0x1F5A4),
    (0x1F5FB, 0x1F64F), // Emoticons
    (0x1F680, 0x1F6C5), // Transport
    (0x1F6CC, 0x1F6CC),
    (0x1F6D0, 0x1F6D2),
    (0x1F6EB, 0x1F6EC),
    (0x1F6F4, 0x1F6FC),
    (0x1F7E0, 0x1F7EB), // Coloured circles and squares
    (0x1F90C, 0x1F9FF), // Supplemental symbols, people
    (0x1FA70, 0x1FAFF),
    (0x20000, 0x2FFFD), // CJK extension B and beyond
    (0x30000, 0x3FFFD),
];

/// Ranges that take no cell of their own: combining marks and format
/// characters. They attach to the preceding cell.
const ZERO: &[(u32, u32)] = &[
    (0x0300, 0x036F), // Combining diacritical marks
    (0x0483, 0x0489),
    (0x0591, 0x05BD),
    (0x0610, 0x061A),
    (0x064B, 0x065F),
    (0x0670, 0x0670),
    (0x06D6, 0x06DC),
    (0x0900, 0x0903),
    (0x093A, 0x093C),
    (0x0941, 0x0948),
    (0x1AB0, 0x1AFF),
    (0x1DC0, 0x1DFF),
    (0x200B, 0x200F), // Zero-width space/joiner, directional marks
    (0x2028, 0x202E),
    (0x20D0, 0x20F0), // Combining marks for symbols
    (0xFE00, 0xFE0F), // Variation selectors
    (0xFE20, 0xFE2F),
    (0xFEFF, 0xFEFF),   // BOM / zero-width no-break space
    (0xE0100, 0xE01EF), // Variation selectors supplement
];

fn in_ranges(table: &[(u32, u32)], c: u32) -> bool {
    table
        .binary_search_by(|&(lo, hi)| {
            if c < lo {
                std::cmp::Ordering::Greater
            } else if c > hi {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// Cells this character occupies: 0, 1 or 2.
///
/// Control characters return 0 — they are executed, never printed, so a caller
/// that reaches here with one has already made a mistake and should not also
/// advance the cursor.
pub fn char_width(c: char) -> usize {
    let v = c as u32;
    if v < 0x20 || (0x7F..0xA0).contains(&v) {
        return 0;
    }
    if v < 0x300 {
        // The overwhelmingly common case: Latin, digits, punctuation. One
        // comparison, no table lookup, because this is the per-character path.
        return 1;
    }
    if in_ranges(ZERO, v) {
        return 0;
    }
    if in_ranges(WIDE, v) {
        return 2;
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_one_cell() {
        for c in [' ', 'a', 'Z', '0', '~', '!'] {
            assert_eq!(char_width(c), 1, "{c:?}");
        }
    }

    #[test]
    fn cjk_and_emoji_are_two_cells() {
        // The bug this module exists to fix: each of these used to take one
        // cell, so a line of them drifted a column per character.
        for c in ['日', '本', '語', '中', '한', 'ア', '　'] {
            assert_eq!(char_width(c), 2, "{c:?} should be double width");
        }
        for c in ['🚀', '✅', '🎉', '🧠'] {
            assert_eq!(char_width(c), 2, "{c:?} should be double width");
        }
    }

    #[test]
    fn halfwidth_katakana_stays_single() {
        // The trap in this table. U+FF01–FF60 are FULLwidth forms and take two
        // cells, but U+FF61–FF9F are HALFwidth katakana and take one — they sit
        // in the same block and look similar. `ア` (U+30A2) is wide; `ｱ`
        // (U+FF71) is not. Getting this backwards misaligns Japanese output in
        // the opposite direction from the bug this module fixes.
        for c in ['ｱ', 'ｲ', 'ｳ', '｡', '｢'] {
            assert_eq!(char_width(c), 1, "{c:?} is halfwidth");
        }
        for c in ['Ａ', '１', '！'] {
            assert_eq!(char_width(c), 2, "{c:?} is fullwidth");
        }
    }

    #[test]
    fn box_drawing_and_symbols_stay_single() {
        // These are the characters `tree`, `systemd` and every TUI use. Getting
        // them wrong would misalign far more output than CJK ever would.
        for c in ['├', '└', '─', '│', '┌', '┐', '┘', '┤', '┬', '┴', '┼'] {
            assert_eq!(char_width(c), 1, "{c:?} must stay single width");
        }
        for c in ['█', '▄', '▀', '░', '▒', '▓', '●', '○', '◆', '▶'] {
            assert_eq!(char_width(c), 1, "{c:?} must stay single width");
        }
        for c in ['↑', '↓', '←', '→', '⇄', '⇅', '…', '·', '✓', '✗', '⚠'] {
            assert_eq!(char_width(c), 1, "{c:?} must stay single width");
        }
    }

    #[test]
    fn accented_latin_stays_single() {
        for c in ['é', 'ü', 'ñ', 'å', 'ø', 'ß', 'œ'] {
            assert_eq!(char_width(c), 1, "{c:?}");
        }
    }

    #[test]
    fn combining_marks_take_no_cell() {
        assert_eq!(char_width('\u{0301}'), 0); // combining acute
        assert_eq!(char_width('\u{200B}'), 0); // zero-width space
        assert_eq!(char_width('\u{FE0F}'), 0); // variation selector-16
    }

    #[test]
    fn control_characters_take_no_cell() {
        for c in ['\n', '\r', '\t', '\x07', '\x1b', '\x7f'] {
            assert_eq!(char_width(c), 0, "{c:?}");
        }
    }

    #[test]
    fn the_range_tables_are_sorted_and_disjoint() {
        // binary_search_by depends on both properties; an unsorted table fails
        // silently by reporting the wrong width for some characters.
        for table in [WIDE, ZERO] {
            for w in table.windows(2) {
                let (a, b) = (w[0], w[1]);
                assert!(a.0 <= a.1, "reversed range {a:?}");
                assert!(a.1 < b.0, "overlapping or unsorted: {a:?} then {b:?}");
            }
        }
    }
}
