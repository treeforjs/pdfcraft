//! Standard-14 metrics from the already attributed hayro data (AGENTS.md §1.1).
//! No font program or proprietary visual asset is embedded here.

mod data {
    include!(concat!(env!("OUT_DIR"), "/standard_metrics.rs"));
}

/// Glyph advance in em units for an exact standard-font name. Unknown fonts/glyphs return
/// `None`; callers may explicitly choose a fallback. This is glyph width, not kerning/shaping.
pub fn standard_glyph_width(base: &str, glyph: &str) -> Option<f64> {
    let table = match base {
        "Helvetica" => data::HELVETICA,
        "Helvetica-Bold" => data::HELVETICA_BOLD,
        "Helvetica-Oblique" => data::HELVETICA_OBLIQUE,
        "Helvetica-BoldOblique" => data::HELVETICA_BOLD_OBLIQUE,
        "Times-Roman" => data::TIMES_ROMAN,
        "Times-Bold" => data::TIMES_BOLD,
        "Times-Italic" => data::TIMES_ITALIC,
        "Times-BoldItalic" => data::TIMES_BOLD_ITALIC,
        "Courier" => data::COURIER,
        "Courier-Bold" => data::COURIER_BOLD,
        "Courier-Oblique" => data::COURIER_OBLIQUE,
        "Courier-BoldOblique" => data::COURIER_BOLD_OBLIQUE,
        "Symbol" => data::SYMBOL,
        "ZapfDingbats" => data::ZAPF_DING_BATS,
        _ => return None,
    };
    let i = table.binary_search_by_key(&glyph, |(name, _)| *name).ok()?;
    table.get(i).map(|(_, width)| f64::from(*width) / 1000.0)
}

/// Exact advance of a Unicode character in a standard Latin font's encoding.
pub fn standard_char_width(base: &str, ch: char) -> Option<f64> {
    let ch = match ch {
        '\u{a0}' => ' ',
        '\u{ad}' => '-',
        _ => ch,
    };
    let glyph = crate::encodings::NAMES.iter().find(|(_, value)| *value == ch as u32)?.0;
    standard_glyph_width(base, glyph)
}

/// Width of text in one of the twelve standard Latin faces, without kerning.
pub fn standard_text_width(base: &str, text: &str, size: f64) -> Option<f64> {
    text.chars().map(|ch| standard_char_width(base, ch).map(|w| w * size)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_distinguish_faces_and_cover_win_ansi() {
        assert_eq!(standard_text_width("Times-Roman", "MW", 10.0), Some(18.33));
        assert_eq!(standard_text_width("Times-Italic", "MW", 10.0), Some(16.66));
        assert_eq!(standard_text_width("Helvetica", "A", 10.0), Some(6.67));
        assert_eq!(standard_text_width("Helvetica-Bold", "A", 10.0), Some(7.22));
        for base in [
            "Helvetica",
            "Helvetica-Bold",
            "Helvetica-Oblique",
            "Helvetica-BoldOblique",
            "Times-Roman",
            "Times-Bold",
            "Times-Italic",
            "Times-BoldItalic",
            "Courier",
            "Courier-Bold",
            "Courier-Oblique",
            "Courier-BoldOblique",
        ] {
            for u in crate::encodings::WIN_ANSI.iter().copied().filter(|u| *u != 0) {
                let ch = char::from_u32(u).unwrap();
                assert!(standard_char_width(base, ch).is_some(), "{base}: U+{u:04X}");
            }
        }
        assert_eq!(standard_text_width("Helvetica", "😀", 12.0), None);
    }
}
