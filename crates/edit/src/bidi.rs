//! Right-to-left text in Edit PDF: display order by the Unicode bidirectional algorithm
//! (UAX #9), visual caret movement, and Arabic presentation forms.
//!
//! PDF pages store text in display order: a right-to-left line is shown glyph by glyph from
//! the left. [`unit_order`] turns a line read that way into logical order (the order it is
//! typed and edited in) and turns a logical row back into display order for writing, so an
//! edited Arabic or Hebrew paragraph reads back as it was typed. Arabic letters join: a font
//! whose ToUnicode maps its glyphs to presentation forms (U+FB50–U+FEFC) takes [`present`]ed
//! text, and extracted presentation forms are read back as letters with [`normalize`].
//!
//! Everything here is total: hostile text (unbalanced or deeply nested embeddings, isolates
//! and overrides, huge runs) gets an order, never a panic. Past [`MAX_BIDI_CHARS`] characters
//! text keeps its logical order.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::OnceLock;

use unicode_bidi::{BidiClass, Level, ParagraphBidiInfo, bidi_class};
use unicode_normalization::UnicodeNormalization as _;

/// The most characters reordered at once; longer text keeps its logical order.
pub const MAX_BIDI_CHARS: usize = 1_000_000;

/// The most characters a caret moves through visually; past it the caret moves logically.
const MAX_CARET_CHARS: usize = 200_000;

/// A strong right-to-left character (Hebrew, Arabic, Syriac, Thaana, N'Ko, …).
pub fn is_rtl(c: char) -> bool {
    matches!(bidi_class(c), BidiClass::R | BidiClass::AL)
}

/// Whether `text` has any strong right-to-left character.
pub fn has_rtl(text: &str) -> bool {
    text.chars().any(is_rtl)
}

/// Direction marks, embeddings, overrides and isolates: they steer the order and are not drawn.
pub fn is_control(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// A paragraph separator (bidi class B): each paragraph is ordered on its own.
fn is_separator(c: char) -> bool {
    bidi_class(c) == BidiClass::B
}

/// Whether a paragraph's base direction is right to left: its first strong character outside
/// isolates (UAX #9 P2–P3). Text without one is left to right.
pub fn base_rtl(text: &str) -> bool {
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '\u{2066}'..='\u{2068}' => depth = depth.saturating_add(1),
            '\u{2069}' => depth = depth.saturating_sub(1),
            _ if depth > 0 => {}
            _ if is_separator(c) => return false,
            _ => match bidi_class(c) {
                BidiClass::L => return false,
                BidiClass::R | BidiClass::AL => return true,
                _ => {}
            },
        }
    }
    false
}

/// One embedding level per character of `text` (which holds no paragraph separator), as a line
/// shows it (rules L1). `None` if the bidi implementation fails.
fn levels(text: &str, rtl: bool) -> Option<Vec<Level>> {
    if text.is_empty() {
        return Some(Vec::new());
    }
    let base = if rtl { Level::rtl() } else { Level::ltr() };
    // The library must not take the editor down on text it can't handle.
    std::panic::catch_unwind(|| {
        let info = ParagraphBidiInfo::new(text, Some(base));
        info.reordered_levels_per_char(0..text.len())
    })
    .ok()
}

/// The visual order of units with these levels: the unit shown at each position (rule L2).
fn reorder(levels: &[Level]) -> Vec<usize> {
    if levels.iter().all(|l| l.is_ltr()) {
        return (0..levels.len()).collect();
    }
    std::panic::catch_unwind(|| ParagraphBidiInfo::reorder_visual(levels)).unwrap_or_else(|_| (0..levels.len()).collect())
}

/// The display order of a run of units (glyphs, each with its text) on one line: for each
/// position from the left, the index of the unit shown there. `rtl` is the paragraph's base
/// direction. Applied to units in logical order it gives display order; applied to units in
/// display order (a line read from a page) it gives their logical order, since reversing runs
/// by level undoes itself. A unit takes the level of its first character; one without text
/// (a positioning gap) the level of the unit before it.
pub fn unit_order(units: &[&str], rtl: bool) -> Vec<usize> {
    let identity = || (0..units.len()).collect::<Vec<_>>();
    let total = units.iter().fold(0usize, |n, u| n.saturating_add(u.len()));
    if total > MAX_BIDI_CHARS.saturating_mul(4) || !(rtl || units.iter().any(|u| has_rtl(u))) {
        return identity();
    }
    // Separators inside a line read as spaces (the line is one paragraph).
    let mut text = String::with_capacity(total);
    let mut firsts = Vec::with_capacity(units.len());
    let mut chars = 0usize;
    for unit in units {
        firsts.push((!unit.is_empty()).then_some(chars));
        for c in unit.chars() {
            text.push(if is_separator(c) { ' ' } else { c });
            chars = chars.saturating_add(1);
        }
    }
    if chars > MAX_BIDI_CHARS {
        return identity();
    }
    let Some(levels) = levels(&text, rtl) else { return identity() };
    let base = if rtl { Level::rtl() } else { Level::ltr() };
    let mut unit_levels = Vec::with_capacity(units.len());
    let mut previous = base;
    for first in firsts {
        let level = first.and_then(|i| levels.get(i).copied()).unwrap_or(previous);
        unit_levels.push(level);
        previous = level;
    }
    reorder(&unit_levels)
}

/// The base direction of a line read from a page in display order. A right-to-left paragraph
/// starts at the right, so when the outermost strong characters agree they decide (both right
/// to left, or both left to right); otherwise the line is right to left when it has at least as
/// many strong right-to-left characters as left-to-right ones. (P2 doesn't apply: the first
/// strong character in display order is the logically last word of a right-to-left line.)
pub fn display_order_rtl(text: &str) -> bool {
    let strong = |c: &char| match bidi_class(*c) {
        BidiClass::R | BidiClass::AL => Some(true),
        BidiClass::L => Some(false),
        _ => None,
    };
    let (left, right) = (text.chars().find_map(|c| strong(&c)), text.chars().rev().find_map(|c| strong(&c)));
    match (left, right) {
        (Some(a), Some(b)) if a == b => a,
        (None, _) | (_, None) => false,
        _ => {
            let (mut rtl, mut ltr) = (0usize, 0usize);
            for c in text.chars() {
                match strong(&c) {
                    Some(true) => rtl = rtl.saturating_add(1),
                    Some(false) => ltr = ltr.saturating_add(1),
                    None => {}
                }
            }
            rtl >= ltr
        }
    }
}

/// Character ranges of the paragraphs of `text` (split at separators, which belong to none).
fn paragraphs(chars: &[char]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in chars.iter().enumerate() {
        if is_separator(*c) {
            out.push((start, i));
            start = i + 1;
        }
    }
    out.push((start, chars.len()));
    out
}

/// For each display position of `text`, the logical character shown there, each paragraph
/// ordered by its own base direction (`rtl`, or each paragraph's first strong character when
/// `None`). Separators stay where they are.
pub fn visual_order(text: &str, rtl: Option<bool>) -> Vec<usize> {
    let chars: Vec<char> = text.chars().take(MAX_BIDI_CHARS.saturating_add(1)).collect();
    if chars.len() > MAX_BIDI_CHARS || (!chars.iter().any(|c| is_rtl(*c)) && rtl != Some(true)) {
        return (0..text.chars().count()).collect();
    }
    let mut out = Vec::with_capacity(chars.len());
    for (start, end) in paragraphs(&chars) {
        let piece: String = chars.get(start..end).unwrap_or_default().iter().collect();
        let base = rtl.unwrap_or_else(|| base_rtl(&piece));
        let order = levels(&piece, base).map(|l| reorder(&l)).unwrap_or_else(|| (0..end - start).collect());
        out.extend(order.into_iter().map(|i| start + i));
        if end < chars.len() {
            out.push(end);
        }
    }
    out
}

/// `text` in display order without its direction controls (for showing it the way the page
/// will). Brackets are not mirrored.
pub fn visual_text(text: &str, rtl: Option<bool>) -> String {
    let chars: Vec<char> = text.chars().collect();
    visual_order(text, rtl).into_iter().filter_map(|i| chars.get(i).copied()).filter(|c| !is_control(*c)).collect()
}

/// Where a caret at logical position `caret` (0 ..= characters) goes when moved one step left
/// (`right == false`) or right on screen, by the display order of its paragraph (`rtl` as for
/// [`visual_order`]). A caret sits on the side of its character that the reading direction
/// meets first: the left edge of a left-to-right character, the right edge of a right-to-left
/// one. Moving past either end of a paragraph steps into the next or previous one. Positions
/// out of range are clamped.
pub fn caret_move(text: &str, caret: usize, right: bool, rtl: Option<bool>) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let caret = caret.min(n);
    let Some(&(start, end)) = paragraphs(&chars).iter().find(|(s, e)| *s <= caret && caret <= *e) else { return caret };
    let piece: String = chars.get(start..end).unwrap_or_default().iter().collect();
    let base = rtl.unwrap_or_else(|| base_rtl(&piece));
    // Moving with the paragraph's reading direction is moving forwards.
    let forward = right != base;
    let step_out = |forward: bool| {
        if forward && end < n {
            end + 1
        } else if !forward && start > 0 {
            start - 1
        } else {
            caret
        }
    };
    let len = end - start;
    let levels = if len > MAX_CARET_CHARS { None } else { levels(&piece, base) };
    let Some(levels) = levels else {
        // Too long to order: move logically.
        return match (forward, caret) {
            (true, c) if c < end => c + 1,
            (false, c) if c > start => c - 1,
            _ => step_out(forward),
        };
    };
    let order = reorder(&levels);
    let mut column = vec![0usize; len];
    for (v, l) in order.iter().enumerate() {
        if let Some(slot) = column.get_mut(*l) {
            *slot = v;
        }
    }
    let rtl_at = |i: usize| levels.get(i).is_some_and(|l| l.is_rtl());
    // Each caret's place in display order: between columns, 0 ..= len.
    let place = |p: usize| -> usize {
        if p < len {
            let v = column.get(p).copied().unwrap_or(0);
            if rtl_at(p) { v + 1 } else { v }
        } else if len == 0 {
            0
        } else {
            let v = column.get(len - 1).copied().unwrap_or(0);
            if rtl_at(len - 1) { v } else { v + 1 }
        }
    };
    let mut carets: Vec<(usize, usize)> = (0..=len).map(|p| (place(p), p)).collect();
    carets.sort_unstable();
    let local = caret - start;
    let Some(at) = carets.iter().position(|(_, p)| *p == local) else { return caret };
    let next = if right { at.checked_add(1).filter(|i| *i < carets.len()) } else { at.checked_sub(1) };
    match next.and_then(|i| carets.get(i)) {
        Some((_, p)) => start + p,
        None => step_out(forward),
    }
}

/// Joining forms of an Arabic letter from the Presentation Forms blocks.
#[derive(Clone, Copy, Debug)]
struct Forms {
    isolated: char,
    end: Option<char>,
    start: Option<char>,
    middle: Option<char>,
}

struct Tables {
    forms: HashMap<char, Forms>,
    /// Lam + alef ligatures: (isolated, final) by the alef.
    lam_alef: HashMap<char, [char; 2]>,
}

/// Built once from the compatibility decompositions of U+FE80–U+FEFC and U+FB50–U+FBFF: a
/// letter's forms are consecutive (isolated, final, initial, medial; right-joining letters
/// have only the first two).
fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(|| {
        let mut forms: HashMap<char, Forms> = HashMap::new();
        let mut lam_alef = HashMap::new();
        for range in [0xFE80u32..=0xFEFC, 0xFB50..=0xFBFF] {
            let mut groups: Vec<(String, Vec<char>)> = Vec::new();
            for c in range.filter_map(char::from_u32) {
                let base: String = std::iter::once(c).nfkc().collect();
                if base.chars().eq(std::iter::once(c)) {
                    continue;
                }
                match groups.last_mut() {
                    Some((b, list)) if *b == base => list.push(c),
                    _ => groups.push((base, vec![c])),
                }
            }
            for (base, list) in groups {
                let mut chars = base.chars();
                match (chars.next(), chars.next(), chars.next()) {
                    (Some(letter), None, _) => {
                        forms.entry(letter).or_insert(Forms {
                            isolated: list.first().copied().unwrap_or(letter),
                            end: list.get(1).copied(),
                            start: list.get(2).copied(),
                            middle: list.get(3).copied(),
                        });
                    }
                    (Some('\u{0644}'), Some(alef), None) => {
                        if let (Some(i), Some(f)) = (list.first(), list.get(1)) {
                            lam_alef.entry(alef).or_insert([*i, *f]);
                        }
                    }
                    _ => {}
                }
            }
        }
        Tables { forms, lam_alef }
    })
}

/// Marks between letters don't break joining.
fn transparent(c: char) -> bool {
    bidi_class(c) == BidiClass::NSM
}

/// Whether `c` joins the letter after it (dual-joining, tatweel, ZWJ).
fn joins_forward(c: char) -> bool {
    matches!(c, '\u{0640}' | '\u{200D}') || tables().forms.get(&c).is_some_and(|f| f.start.is_some() && f.middle.is_some())
}

/// Whether `c` joins the letter before it (it has a final form, or is tatweel / ZWJ).
fn joins_backward(c: char) -> bool {
    matches!(c, '\u{0640}' | '\u{200D}') || tables().forms.get(&c).is_some_and(|f| f.end.is_some())
}

/// Arabic letters of `text` (logical order) as the presentation forms their neighbours call
/// for, with lam + alef as one ligature; everything else is unchanged. For fonts whose glyphs
/// are mapped to presentation forms, so typed letters pick the right glyph.
pub fn present(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if !chars.iter().any(|c| tables().forms.contains_key(c)) {
        return text.to_owned();
    }
    let t = tables();
    let neighbour = |from: usize, forward: bool| -> Option<char> {
        let mut i = from;
        loop {
            i = if forward { i.checked_add(1)? } else { i.checked_sub(1)? };
            let c = *chars.get(i)?;
            if !transparent(c) {
                return Some(c);
            }
        }
    };
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        let Some(f) = t.forms.get(&c) else {
            out.push(c);
            i += 1;
            continue;
        };
        let after_joins = neighbour(i, false).is_some_and(joins_forward) && joins_backward(c);
        if c == '\u{0644}'
            && let Some(lig) = chars.get(i + 1).and_then(|a| t.lam_alef.get(a))
        {
            out.push(lig[usize::from(after_joins)]);
            i += 2;
            continue;
        }
        let before_joins = joins_forward(c) && neighbour(i, true).is_some_and(joins_backward);
        out.push(match (after_joins, before_joins) {
            (false, false) => f.isolated,
            (true, false) => f.end.unwrap_or(f.isolated),
            (false, true) => f.start.unwrap_or(f.isolated),
            (true, true) => f.middle.or(f.end).unwrap_or(f.isolated),
        });
        i += 1;
    }
    out
}

/// Whether `c` is an Arabic presentation form (or another compatibility form of the Arabic
/// presentation blocks).
fn presentation_form(c: char) -> bool {
    matches!(u32::from(c), 0xFB50..=0xFDFF | 0xFE70..=0xFEFF)
}

/// Text extracted from a page with presentation forms (`ﺑ`, `ﻻ`) as the letters they show
/// (`ب`, `لا`), so it can be edited and searched like typed text. Other text is borrowed.
pub fn normalize(text: &str) -> Cow<'_, str> {
    if !text.chars().any(presentation_form) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if presentation_form(c) {
            // Isolated harakat decompose to a space and the mark; keep only the mark.
            let base: String = std::iter::once(c).nfkc().collect();
            let trimmed = base.trim_start_matches(' ');
            if trimmed.is_empty() { out.push(c) } else { out.push_str(trimmed) }
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

/// An Arabic letter that joins its neighbours (so editing next to it can change its form).
pub fn joining_letter(c: char) -> bool {
    joins_backward(c) || joins_forward(c)
}

/// Part of a joined Arabic word: a joining letter or a mark on one.
pub fn in_joined_word(c: char) -> bool {
    joining_letter(c) || (transparent(c) && matches!(u32::from(c), 0x0600..=0x06FF | 0x0750..=0x077F | 0x0870..=0x08FF))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(units: &[&str], rtl: bool) -> String {
        unit_order(units, rtl).into_iter().map(|i| units[i]).collect()
    }

    #[test]
    fn base_direction_is_the_first_strong_character() {
        assert!(base_rtl("שלום world"));
        assert!(!base_rtl("hello שלום"));
        assert!(base_rtl("123 مرحبا"));
        assert!(!base_rtl("123"));
        // Isolated text doesn't decide.
        assert!(base_rtl("\u{2066}abc\u{2069} שלום"));
        assert!(display_order_rtl("םולש abc"));
        assert!(!display_order_rtl("abc def ש"));
        assert!(display_order_rtl("םלוע 123 םולש"));
        assert!(!display_order_rtl("abc ש def"));
        assert!(!display_order_rtl("123"));
    }

    #[test]
    fn rows_go_to_display_order_and_back() {
        // Logical "שלום 123 עולם": the number reads left to right inside the right-to-left line.
        let logical: Vec<String> = "שלום 123 עולם".chars().map(String::from).collect();
        let units: Vec<&str> = logical.iter().map(String::as_str).collect();
        let shown = apply(&units, true);
        assert_eq!(shown, "םלוע 123 םולש");
        // Read back from the page in display order, it is logical again.
        let shown_units: Vec<String> = shown.chars().map(String::from).collect();
        let shown_units: Vec<&str> = shown_units.iter().map(String::as_str).collect();
        assert_eq!(apply(&shown_units, display_order_rtl(&shown)), "שלום 123 עולם");
        // Left-to-right text is untouched, and so is an embedded right-to-left word's place.
        assert_eq!(apply(&["a", "b", " ", "א", "ב"], false), "ab בא");
        assert_eq!(apply(&["a", "b"], false), "ab");
        // Gaps (units without text) stay with the unit before them.
        assert_eq!(unit_order(&["א", "", "ב"], true), vec![2, 1, 0]);
    }

    #[test]
    fn caret_moves_visually() {
        // "abc אבג": in a left-to-right paragraph the Hebrew word shows reversed at the right.
        let text = "abc אבג";
        // Right of the space the caret is at the Hebrew word's visual left: after its last
        // letter, logical 7.
        assert_eq!(caret_move(text, 3, true, None), 7);
        assert_eq!(caret_move(text, 7, true, None), 6);
        assert_eq!(caret_move(text, 7, false, None), 3);
        let mut seen = vec![0];
        let mut c = 0;
        for _ in 0..10 {
            c = caret_move(text, c, true, None);
            seen.push(c);
        }
        // Every position is visited moving right, ending at the visual right end.
        let mut sorted = seen.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, (0..=7).collect::<Vec<_>>(), "{seen:?}");
        // Pure Hebrew: right arrow moves logically backwards, left forwards.
        assert_eq!(caret_move("אבג", 1, false, None), 2);
        assert_eq!(caret_move("אבג", 1, true, None), 0);
        assert_eq!(caret_move("אבג", 0, true, None), 0);
        assert_eq!(caret_move("abc", 1, true, None), 2);
        // Out of range clamps; empty text stays put; paragraphs are crossed.
        assert_eq!(caret_move("abc", 99, true, None), 3);
        assert_eq!(caret_move("", 0, true, None), 0);
        assert_eq!(caret_move("ab\ncd", 2, true, None), 3);
        assert_eq!(caret_move("ab\ncd", 3, false, None), 2);
    }

    #[test]
    fn hostile_controls_and_huge_runs_never_panic() {
        let nested: String = "\u{202B}".repeat(500) + "abc" + &"\u{2067}".repeat(300) + "אבג" + &"\u{202C}".repeat(900) + "\u{2069}";
        let order = visual_order(&nested, None);
        assert_eq!(order.len(), nested.chars().count());
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..order.len()).collect::<Vec<_>>());
        for i in [0, 3, 500, 900, nested.chars().count()] {
            let _ = caret_move(&nested, i, true, None);
            let _ = caret_move(&nested, i, false, Some(true));
        }
        assert!(!visual_text("\u{200F}\u{202E}abc\u{202C}", None).chars().any(is_control));
        let huge = "א".repeat(MAX_BIDI_CHARS + 5);
        assert_eq!(visual_order(&huge, None).first(), Some(&0));
        let units: Vec<&str> = vec!["א"; 300_000];
        assert_eq!(unit_order(&units, true).first(), Some(&299_999));
        assert_eq!(caret_move(&"א".repeat(MAX_CARET_CHARS + 2), 5, false, None), 6);
        assert_eq!(unit_order(&[], true), Vec::<usize>::new());
    }

    #[test]
    fn arabic_letters_take_presentation_forms_and_read_back() {
        // بيت: initial beh, medial yeh, final teh.
        assert_eq!(present("بيت"), "\u{FE91}\u{FEF4}\u{FE96}");
        // دار: dal and alef don't join forwards, so every letter stands alone.
        assert_eq!(present("دار"), "\u{FEA9}\u{FE8D}\u{FEAD}");
        assert_eq!(present("باب"), "\u{FE91}\u{FE8E}\u{FE8F}");
        // Lam-alef is one ligature; a mark doesn't break joining.
        assert_eq!(present("لا"), "\u{FEFB}");
        assert_eq!(present("سلام"), "\u{FEB3}\u{FEFC}\u{FEE1}");
        assert_eq!(present("بَت"), "\u{FE91}\u{064E}\u{FE96}");
        assert_eq!(present("abc"), "abc");
        assert_eq!(normalize(&present("سلام بيت")), "سلام بيت");
        assert_eq!(normalize("\u{FE70}"), "\u{064B}");
        assert!(matches!(normalize("abc"), Cow::Borrowed(_)));
        assert!(joining_letter('ب') && joining_letter('د') && !joining_letter('a'));
    }
}
