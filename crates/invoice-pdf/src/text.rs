//! WinAnsi byte encoding for the printed-invoice surface.
//!
//! The renderer uses the built-in PDF font `Helvetica` with
//! `WinAnsiEncoding`. WinAnsi covers Latin-1 + the Microsoft extension
//! range (0x80-0x9F: smart quotes, dashes, `€`, etc.); it does NOT
//! cover the Hungarian-specific double-acute characters `ő/ű/Ő/Ű`
//! (those live in Latin-2 / Unicode U+0150/U+0151/U+0170/U+0171). The
//! reference template's Hungarian text includes these glyphs in words
//! like "időszak", "követelés", "összeg", "törvény", "végösszeg".
//!
//! # The substitution decision (A-decision recorded in
//! `_handoffs/56-session-56-close.md`)
//!
//! Per CLAUDE.md rule 12 (fail loud) + rule 2 (simplicity first):
//! - We substitute `ő → ö` and `ű → ü` at the byte-emit boundary,
//!   matching the visually-closest WinAnsi-covered diacritic.
//! - The substitution is documented LOUD inline (this module) so a
//!   future reader sees it immediately.
//! - The PR-44ε.2 deferred row in the session-56 close handoff names
//!   "proper Unicode font embedding (Type0/CIDFontType2 with Identity-H
//!   encoding)" as the fix — the renderer's WinAnsi posture is the
//!   surface that PR-44ε.2 replaces. The substitution is OBSERVABLE in
//!   the rendered PDF; it is NOT a silent loss.
//!
//! Why not embed a Unicode font now: per the brief's "ship what fits
//! and name the deferred piece" — Type0 font embedding is ~300 LoC of
//! CIDFontType2 glue (font subsetting, ToUnicode cmap, glyph-index
//! lookup via ttf-parser) that bloats THIS PR substantially. A152
//! records the trade-off; PR-44ε.2 lifts it.

/// Map a Unicode `char` to its WinAnsi byte, substituting Hungarian
/// double-acute characters to their WinAnsi-covered single-acute
/// equivalents. Unknown chars (anything outside ASCII + Latin-1 +
/// WinAnsi's 0x80-0x9F supplement, and the post-substitution
/// Hungarian set) emit `0x3F` (`?`) — visible to the reader, not a
/// silent drop.
///
/// The WinAnsi code-point assignments below come from the Adobe
/// WinAnsiEncoding spec (a near-superset of CP-1252).
pub fn winansi_byte_for_char(c: char) -> u8 {
    match c {
        // ASCII identity range.
        c if (c as u32) < 0x80 => c as u8,

        // Hungarian double-acute substitutions per A152. Documented
        // LOUD in this module's preamble; PR-44ε.2 lifts via font
        // embedding.
        '\u{0150}' => 0xD6, // Ő → Ö
        '\u{0151}' => 0xF6, // ő → ö
        '\u{0170}' => 0xDC, // Ű → Ü
        '\u{0171}' => 0xFC, // ű → ü

        // WinAnsi 0x80-0x9F supplement — the codepoints that diverge
        // from pure Latin-1.
        '\u{20AC}' => 0x80, // €
        '\u{201A}' => 0x82, // ‚
        '\u{0192}' => 0x83, // ƒ
        '\u{201E}' => 0x84, // „
        '\u{2026}' => 0x85, // …
        '\u{2020}' => 0x86, // †
        '\u{2021}' => 0x87, // ‡
        '\u{02C6}' => 0x88, // ˆ
        '\u{2030}' => 0x89, // ‰
        '\u{0160}' => 0x8A, // Š
        '\u{2039}' => 0x8B, // ‹
        '\u{0152}' => 0x8C, // Œ
        '\u{017D}' => 0x8E, // Ž
        '\u{2018}' => 0x91, // ‘
        '\u{2019}' => 0x92, // ’
        '\u{201C}' => 0x93, // “
        '\u{201D}' => 0x94, // ”
        '\u{2022}' => 0x95, // •
        '\u{2013}' => 0x96, // –
        '\u{2014}' => 0x97, // —
        '\u{02DC}' => 0x98, // ˜
        '\u{2122}' => 0x99, // ™
        '\u{0161}' => 0x9A, // š
        '\u{203A}' => 0x9B, // ›
        '\u{0153}' => 0x9C, // œ
        '\u{017E}' => 0x9E, // ž
        '\u{0178}' => 0x9F, // Ÿ

        // Latin-1 range — byte values are the same as Unicode code
        // points in 0xA0-0xFF.
        c if (c as u32) >= 0xA0 && (c as u32) <= 0xFF => c as u8,

        // Anything else: visible question mark per CLAUDE.md rule 12.
        _ => b'?',
    }
}

/// Convert a `&str` into the WinAnsi byte sequence the PDF content
/// stream's `Tj` operator consumes.
pub fn winansi_bytes(s: &str) -> Vec<u8> {
    s.chars().map(winansi_byte_for_char).collect()
}

/// Helvetica advance widths in 1/1000 em for the WinAnsi printable
/// ASCII range `0x20..=0x7E`. Source: Adobe Core-14 AFM
/// (`Helvetica.afm`). Index = `byte - 0x20`.
///
/// PR-249 — added to back the two-column header wrap (Bug A). The
/// pre-existing `0.55 * size` per-char proxy in `lib.rs::text_right`
/// underestimates the width of an all-caps legal name (caps average
/// ≈ 0.68 em, not 0.55), so a proxy-based clamp would still let an
/// all-caps `ÁBEN CONSULTING KORLÁTOLT FELELŐSSÉGŰ TÁRSASÁG` overflow
/// its column. The header wrap therefore measures with real glyph
/// advances; the totals-block right-alignment keeps the coarser proxy
/// (a deliberate two-model split — see the report / `text_right` doc).
#[rustfmt::skip]
const HELVETICA_W: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, // 0x20-0x2F
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, // 0x30-0x3F
    1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, // 0x40-0x4F
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556, // 0x50-0x5F
    333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556, // 0x60-0x6F
    556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,       // 0x70-0x7E
];

/// Helvetica-Bold advance widths in 1/1000 em for `0x20..=0x7E`.
/// Source: Adobe Core-14 AFM (`Helvetica-Bold.afm`). Index =
/// `byte - 0x20`. Used for the size-13 bold party-name field.
#[rustfmt::skip]
const HELVETICA_BOLD_W: [u16; 95] = [
    278, 333, 474, 556, 556, 889, 722, 238, 333, 333, 389, 584, 278, 333, 278, 278, // 0x20-0x2F
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 333, 333, 584, 584, 584, 611, // 0x30-0x3F
    975, 722, 722, 722, 722, 667, 611, 778, 722, 278, 556, 722, 611, 833, 722, 778, // 0x40-0x4F
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 333, 278, 333, 584, 556, // 0x50-0x5F
    333, 556, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556, 278, 889, 611, 611, // 0x60-0x6F
    611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500, 389, 280, 389, 584,       // 0x70-0x7E
];

/// Map any `char` to a representative WinAnsi printable-ASCII byte for
/// width lookup. Accented Latin-1 letters share their base letter's
/// advance in Helvetica (`Á` == `A` == 667), so we fold to the base.
/// The `ő/ű → ö/ü` substitution and `€`/NBSP are resolved by
/// [`winansi_byte_for_char`] first; remaining accents fold here.
fn width_proxy_byte(c: char) -> u8 {
    let b = winansi_byte_for_char(c);
    match b {
        0x20..=0x7E => b,
        0xA0 => b' ', // NBSP → space advance
        0x80 => b'0', // € → digit-width bucket (556)
        0xC0..=0xC5 => b'A',
        0xC6 => b'M', // Æ — rare; nearest wide cap
        0xC7 => b'C',
        0xC8..=0xCB => b'E',
        0xCC..=0xCF => b'I',
        0xD0 => b'D',
        0xD1 => b'N',
        0xD2..=0xD6 | 0xD8 => b'O',
        0xD9..=0xDC => b'U',
        0xDD => b'Y',
        0xDF => b's', // ß
        0xE0..=0xE5 => b'a',
        0xE6 => b'm', // æ
        0xE7 => b'c',
        0xE8..=0xEB => b'e',
        0xEC..=0xEF => b'i',
        0xF0 | 0xF2..=0xF6 | 0xF8 => b'o',
        0xF1 => b'n',
        0xF9..=0xFC => b'u',
        0xFD | 0xFF => b'y',
        _ => b'o', // default mid-width bucket
    }
}

/// Width of `s` rendered in Helvetica (or Helvetica-Bold) at `size`
/// points, in whole PDF points. Sums real glyph advances (1/1000 em)
/// and scales by `size`. Hungarian accented glyphs measure as their
/// base letter — correct, because Helvetica gives `Á` the same advance
/// as `A`. PR-249 — backs the header column wrap (Bug A).
pub fn text_width_points(s: &str, size: i64, bold: bool) -> i64 {
    let table = if bold {
        &HELVETICA_BOLD_W
    } else {
        &HELVETICA_W
    };
    let thousandths: i64 = s
        .chars()
        .map(|c| table[(width_proxy_byte(c) - 0x20) as usize] as i64)
        .sum();
    thousandths * size / 1000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_identity() {
        assert_eq!(winansi_bytes("Szamla 2026"), b"Szamla 2026".to_vec());
    }

    #[test]
    fn euro_glyph_maps_to_0x80() {
        assert_eq!(
            winansi_bytes("\u{20AC}8 636"),
            vec![0x80, b'8', b' ', b'6', b'3', b'6']
        );
    }

    #[test]
    fn hungarian_single_acute_round_trip() {
        // á é í ó ú ö ü + Á É Í Ó Ú Ö Ü all in WinAnsi Latin-1 range.
        let bytes = winansi_bytes("Számla Összeg Áfa");
        assert_eq!(
            bytes,
            vec![
                b'S', b'z', 0xE1, b'm', b'l', b'a', b' ', 0xD6, b's', b's', b'z', b'e', b'g', b' ',
                0xC1, b'f', b'a',
            ]
        );
    }

    #[test]
    fn hungarian_double_acute_substituted_to_single_acute() {
        // ő → ö (0xF6); ű → ü (0xFC). Per A152 the substitution is
        // intentional and documented loud in the module preamble.
        assert_eq!(winansi_byte_for_char('\u{0151}'), 0xF6);
        assert_eq!(winansi_byte_for_char('\u{0171}'), 0xFC);
        assert_eq!(winansi_byte_for_char('\u{0150}'), 0xD6);
        assert_eq!(winansi_byte_for_char('\u{0170}'), 0xDC);
    }

    #[test]
    fn unknown_codepoint_maps_to_question_mark() {
        assert_eq!(winansi_byte_for_char('\u{4E2D}'), b'?'); // CJK
    }

    #[test]
    fn text_width_scales_with_size() {
        // "AA" at size 10: 2 × 667/1000 × 10 = 13 (bold A is 722 → 14).
        assert_eq!(text_width_points("AA", 10, false), 13);
        assert_eq!(text_width_points("AA", 10, true), 14);
        // Linear in size.
        assert_eq!(
            text_width_points("Consulting", 20, false),
            text_width_points("Consulting", 10, false) * 2
        );
    }

    #[test]
    fn accented_caps_measure_as_base_letter() {
        // PR-249 — the whole point of the metric: Hungarian accented
        // capitals must measure identically to their base ASCII letter
        // (Helvetica gives `Á` the same advance as `A`). A char-count
        // proxy would be blind to glyph width entirely; this asserts
        // the fold is in place so the column clamp is accurate.
        assert_eq!(
            text_width_points("ÁÉÍÓÖŐÚÜŰ", 13, true),
            text_width_points("AEIOOOUUU", 13, true)
        );
    }

    #[test]
    fn nbsp_measures_as_a_space() {
        // Bug B's NBSP must not be measured as zero-width or as the
        // `?` fallback — it carries the regular-space advance.
        assert_eq!(
            text_width_points("\u{00A0}", 20, false),
            text_width_points(" ", 20, false)
        );
    }
}
