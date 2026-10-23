//! Reading order and lists for the flowing formats (Word, HTML, RTF, Excel).
//!
//! **Reading order.** A page's paragraphs, tables and images are put in the order a reader
//! takes them by an original recursive cut of the page: it is split into bands at horizontal
//! gaps no box crosses, and a band into columns at vertical gaps (gutters) when each column is a
//! real share of the band's width (so a label beside its value is not a column). Consecutive
//! bands whose gutters line up are one multi-column stretch, read column by column, so two
//! columns of text whose paragraph breaks happen to line up are not interleaved. A heading or
//! picture across the columns ends the stretch. Within a band without columns, boxes are read
//! row by row (boxes that share most of their height are one row), left to right. The recursion
//! is depth-limited and pages with very many boxes fall back to that row order.
//!
//! **Lists.** A run of two or more body paragraphs that start with a bullet (•, ◦, ▪, –, …) or
//! with the same kind of number (`1.`, `2)`, `(3)`, `a.`, `iv.`) is a list; the indentation
//! of each item's left edge gives its level (up to four). A marker drawn as its own text, left of
//! the item's text on the same row, is taken as the item's marker.

use super::{Block, Item};

/// Deeper cuts than this read in row order.
const MAX_DEPTH: usize = 12;
/// More boxes than this on one page read in row order (the cut is O(n log n) a level).
const MAX_CUT_BOXES: usize = 20_000;
/// A gutter is at least this wide (points).
const MIN_GUTTER: f64 = 6.0;
/// Each column is at least this share of its band's width.
const MIN_COLUMN_SHARE: f64 = 0.18;

/// The gaps between a band's columns, left to right ([x0, x1] each).
type Gutters = Vec<[f64; 2]>;

/// The reading order of boxes `[x0, y0, x1, y1]` (user space, y up), as indices.
pub(crate) fn reading_order(rects: &[[f64; 4]]) -> Vec<usize> {
    let rects: Vec<[f64; 4]> = rects.iter().map(|r| sane(*r)).collect();
    let mut out = Vec::with_capacity(rects.len());
    let all: Vec<usize> = (0..rects.len()).collect();
    if rects.len() > MAX_CUT_BOXES {
        out.extend(rows(&rects, all));
    } else {
        cut(&rects, all, 0, &mut out);
    }
    out
}

/// A box with finite, ordered coordinates (non-finite values read as 0).
fn sane(r: [f64; 4]) -> [f64; 4] {
    let r = r.map(|v| if v.is_finite() { v } else { 0.0 });
    [r[0].min(r[2]), r[1].min(r[3]), r[0].max(r[2]), r[1].max(r[3])]
}

fn cut(rects: &[[f64; 4]], idx: Vec<usize>, depth: usize, out: &mut Vec<usize>) {
    if idx.len() <= 1 || depth >= MAX_DEPTH {
        out.extend(rows(rects, idx));
        return;
    }
    let bands = bands(rects, idx);
    if bands.len() == 1 {
        let Some(band) = bands.into_iter().next() else { return };
        match gutters(rects, &band) {
            Some(g) => {
                for column in split_columns(rects, band, &g) {
                    cut(rects, column, depth + 1, out);
                }
            }
            None => out.extend(rows(rects, band)),
        }
        return;
    }
    // Consecutive bands with lined-up gutters are one multi-column stretch.
    let mut groups: Vec<(Vec<usize>, Option<Gutters>)> = Vec::new();
    for band in bands {
        let g = gutters(rects, &band);
        if let (Some((members, Some(last))), Some(new)) = (groups.last_mut(), &g)
            && let Some(common) = line_up(last, new)
        {
            members.extend(band);
            *last = common;
            continue;
        }
        groups.push((band, g));
    }
    for (members, g) in groups {
        match g {
            Some(g) => {
                for column in split_columns(rects, members, &g) {
                    cut(rects, column, depth + 1, out);
                }
            }
            None => cut(rects, members, depth + 1, out),
        }
    }
}

/// Split boxes into bands top to bottom at horizontal gaps no box crosses.
fn bands(rects: &[[f64; 4]], mut idx: Vec<usize>) -> Vec<Vec<usize>> {
    idx.sort_by(|a, b| top(rects, *b).total_cmp(&top(rects, *a)));
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut floor = f64::INFINITY;
    for i in idx {
        let r = rects.get(i).copied().unwrap_or_default();
        match out.last_mut() {
            Some(band) if r[3] > floor => {
                band.push(i);
                floor = floor.min(r[1]);
            }
            _ => {
                out.push(vec![i]);
                floor = r[1];
            }
        }
    }
    out
}

fn top(rects: &[[f64; 4]], i: usize) -> f64 {
    rects.get(i).map_or(0.0, |r| r[3])
}

/// The gutters of a band that splits into real columns (see the module notes), left to right.
fn gutters(rects: &[[f64; 4]], band: &[usize]) -> Option<Vec<[f64; 2]>> {
    if band.len() < 2 {
        return None;
    }
    let mut spans: Vec<[f64; 2]> = band.iter().filter_map(|i| rects.get(*i)).map(|r| [r[0], r[2]]).collect();
    spans.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let (left, right) = (spans.first()?[0], spans.iter().map(|s| s[1]).fold(f64::MIN, f64::max));
    let width = right - left;
    if width <= 0.0 {
        return None;
    }
    let mut gaps = Vec::new();
    let mut reach = spans.first()?[1];
    for s in spans.iter().skip(1) {
        if s[0] - reach >= MIN_GUTTER {
            gaps.push([reach, s[0]]);
        }
        reach = reach.max(s[1]);
    }
    if gaps.is_empty() {
        return None;
    }
    // Every column must be a real share of the band.
    let mut edges = vec![left];
    for g in &gaps {
        edges.push(g[0]);
        edges.push(g[1]);
    }
    edges.push(right);
    let real = edges.chunks(2).all(|c| c.len() == 2 && c[1] - c[0] >= width * MIN_COLUMN_SHARE);
    real.then_some(gaps)
}

/// The gutters two bands share (each pair overlapping), narrowed to the overlap.
fn line_up(a: &[[f64; 2]], b: &[[f64; 2]]) -> Option<Vec<[f64; 2]>> {
    if a.len() != b.len() {
        return None;
    }
    a.iter()
        .zip(b)
        .map(|(x, y)| {
            let (l, r) = (x[0].max(y[0]), x[1].min(y[1]));
            (r > l).then_some([l, r])
        })
        .collect()
}

/// Boxes by the column (between `gutters`) their centre falls in, left to right.
fn split_columns(rects: &[[f64; 4]], idx: Vec<usize>, gutters: &[[f64; 2]]) -> Vec<Vec<usize>> {
    let mut columns: Vec<Vec<usize>> = vec![Vec::new(); gutters.len() + 1];
    for i in idx {
        let r = rects.get(i).copied().unwrap_or_default();
        let centre = (r[0] + r[2]) / 2.0;
        let k = gutters.iter().filter(|g| (g[0] + g[1]) / 2.0 < centre).count();
        if let Some(c) = columns.get_mut(k) {
            c.push(i);
        }
    }
    columns.retain(|c| !c.is_empty());
    columns
}

/// Row order: top to bottom, boxes that share most of their height as one row, left to right.
fn rows(rects: &[[f64; 4]], mut idx: Vec<usize>) -> Vec<usize> {
    idx.sort_by(|a, b| top(rects, *b).total_cmp(&top(rects, *a)));
    let mut out = Vec::with_capacity(idx.len());
    let mut row: Vec<usize> = Vec::new();
    let flush = |row: &mut Vec<usize>, out: &mut Vec<usize>| {
        row.sort_by(|a, b| rects.get(*a).map_or(0.0, |r| r[0]).total_cmp(&rects.get(*b).map_or(0.0, |r| r[0])));
        out.append(row);
    };
    for i in idx {
        let r = rects.get(i).copied().unwrap_or_default();
        let same = row.first().and_then(|f| rects.get(*f)).is_some_and(|f| {
            let overlap = f[3].min(r[3]) - f[1].max(r[1]);
            overlap > 0.5 * (f[3] - f[1]).min(r[3] - r[1]).max(0.0)
        });
        if !same {
            flush(&mut row, &mut out);
        }
        row.push(i);
    }
    flush(&mut row, &mut out);
    out
}

/// How a list's items are numbered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Numbering {
    Bullet,
    Decimal,
    LowerLetter,
    UpperLetter,
    LowerRoman,
    UpperRoman,
}

/// What follows the number: `1.`, `1)` or `(1)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delimiter {
    Period,
    Paren,
    Parens,
}

/// A list item's marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Marker {
    pub numbering: Numbering,
    pub delimiter: Delimiter,
    /// The item's number (1 for bullets).
    pub value: u32,
    /// Byte length of the marker and the space after it.
    pub len: usize,
}

const BULLETS: &[char] = &['•', '◦', '▪', '▫', '‣', '⁃', '●', '○', '■', '□', '–', '—', '-', '*', '·', '➢', '►', '▶', '✓', '✔', '◆', '◇', '\u{F0B7}'];

/// The marker `text` starts with, if any. `alone`: the text is only the marker (drawn apart from
/// the item's text), so no space has to follow.
pub(crate) fn marker(text: &str, alone: bool) -> Option<Marker> {
    let first = text.chars().next()?;
    let after = |len: usize| -> Option<usize> {
        let rest = text.get(len..)?;
        if alone {
            return rest.trim().is_empty().then_some(text.len());
        }
        let ws: usize = rest.chars().take_while(|c| c.is_whitespace()).map(char::len_utf8).sum();
        // Text must follow the marker and a space.
        (ws > 0 && !rest.get(ws..)?.trim().is_empty()).then_some(len + ws)
    };
    if BULLETS.contains(&first) {
        let len = after(first.len_utf8())?;
        return Some(Marker { numbering: Numbering::Bullet, delimiter: Delimiter::Period, value: 1, len });
    }
    let (open, body_start) = if first == '(' { (true, 1) } else { (false, 0) };
    let rest = text.get(body_start..)?;
    let token: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric()).take(6).collect();
    if token.is_empty() {
        return None;
    }
    let close = rest.get(token.len()..)?.chars().next()?;
    let delimiter = match (open, close) {
        (true, ')') => Delimiter::Parens,
        (false, ')') => Delimiter::Paren,
        (false, '.') => Delimiter::Period,
        _ => return None,
    };
    let len = after(body_start + token.len() + 1)?;
    let (numbering, value) = if token.bytes().all(|b| b.is_ascii_digit()) {
        if token.len() > 3 {
            return None;
        }
        (Numbering::Decimal, token.parse().ok()?)
    } else if let Some(v) = roman(&token) {
        let upper = token.bytes().all(|b| b.is_ascii_uppercase());
        // A lone letter other than i, v or x is a letter (c. is "c", not 100).
        if token.len() == 1 && !matches!(token.as_str(), "i" | "I" | "v" | "V" | "x" | "X") {
            letter(&token)?
        } else {
            (if upper { Numbering::UpperRoman } else { Numbering::LowerRoman }, v)
        }
    } else {
        letter(&token)?
    };
    Some(Marker { numbering, delimiter, value, len })
}

fn letter(token: &str) -> Option<(Numbering, u32)> {
    let mut chars = token.chars();
    let c = chars.next()?;
    if chars.next().is_some() || !c.is_ascii_alphabetic() {
        return None;
    }
    let numbering = if c.is_ascii_uppercase() { Numbering::UpperLetter } else { Numbering::LowerLetter };
    Some((numbering, u32::from(c.to_ascii_lowercase() as u8 - b'a') + 1))
}

/// A roman numeral's value (up to 3999), case-insensitive and in one case.
fn roman(token: &str) -> Option<u32> {
    let upper = token.bytes().all(|b| b.is_ascii_uppercase());
    let lower = token.bytes().all(|b| b.is_ascii_lowercase());
    if !(upper || lower) {
        return None;
    }
    let digit = |c: u8| match c.to_ascii_lowercase() {
        b'i' => Some(1),
        b'v' => Some(5),
        b'x' => Some(10),
        b'l' => Some(50),
        b'c' => Some(100),
        b'd' => Some(500),
        b'm' => Some(1000),
        _ => None,
    };
    let values: Vec<u32> = token.bytes().map(digit).collect::<Option<_>>()?;
    // At most six digits of at most 1000: no overflow in i64.
    let mut total: i64 = 0;
    for (k, v) in values.iter().enumerate() {
        if values.get(k + 1).is_some_and(|n| n > v) {
            total -= i64::from(*v);
        } else {
            total += i64::from(*v);
        }
    }
    let total = u32::try_from(total).ok()?;
    // Only the canonical spelling ("iv", not "iiii") counts.
    (total > 0 && total < 4000 && to_roman(total) == token.to_ascii_lowercase()).then_some(total)
}

fn to_roman(mut n: u32) -> String {
    let mut s = String::new();
    for (v, r) in [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ] {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    s
}

/// One item of a list in the output.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ListItem<'a> {
    pub block: &'a Block,
    /// Which list of the document it belongs to (lists number from 0).
    pub list: usize,
    pub level: u8,
    pub numbering: Numbering,
    pub delimiter: Delimiter,
    /// The first item's number.
    pub start: u32,
    /// Bytes of `block.text` that are the marker (the item's text follows).
    pub skip: usize,
}

/// Turn runs of list paragraphs in `items` into list items (see the module notes). `next_list`
/// numbers the document's lists across pages.
pub(crate) fn lists<'a>(items: Vec<Item<'a>>, next_list: &mut usize) -> Vec<Item<'a>> {
    // First fold markers drawn apart (a lone "•" or "1." left of its text on the same row).
    let mut folded: Vec<(Item<'a>, Option<Marker>)> = Vec::with_capacity(items.len());
    let mut it = items.into_iter().peekable();
    while let Some(item) = it.next() {
        if let Item::Para(b, 0) = &item
            && let Some(m) = marker(b.text.trim(), true)
            && let Some(Item::Para(next, 0)) = it.peek()
            && same_row(b, next)
            && next.rect[0] >= b.rect[2] - 1.0
        {
            let next = *next;
            it.next();
            // The marker paragraph is dropped: the list draws its own.
            folded.push((Item::Para(next, 0), Some(Marker { len: 0, ..m })));
            continue;
        }
        let m = match &item {
            Item::Para(b, 0) => marker(&b.text, false),
            _ => None,
        };
        folded.push((item, m));
    }
    let mut out = Vec::with_capacity(folded.len());
    let mut k = 0;
    while k < folded.len() {
        // A run of items with markers of compatible kinds.
        let kind = |m: &Marker| m.numbering == Numbering::Bullet;
        let start = k;
        if let Some(first) = folded.get(k).and_then(|f| f.1) {
            let mut end = k + 1;
            while folded.get(end).and_then(|f| f.1).is_some_and(|m| kind(&m) == kind(&first)) {
                end += 1;
            }
            if end - start >= 2 {
                let left = folded[start..end].iter().filter_map(|(i, _)| para_left(i)).fold(f64::INFINITY, f64::min);
                let list = *next_list;
                *next_list += 1;
                for (item, m) in folded.drain(start..end).collect::<Vec<_>>() {
                    let (Item::Para(b, _), Some(m)) = (item, m) else { continue };
                    let indent = (b.rect[0] - left).max(0.0);
                    let level = if indent.is_finite() { (indent / 18.0).round().clamp(0.0, 3.0) as u8 } else { 0 };
                    out.push(Item::List(ListItem {
                        block: b,
                        list,
                        level,
                        numbering: first.numbering,
                        delimiter: first.delimiter,
                        start: first.value,
                        skip: m.len,
                    }));
                }
                k = start;
                continue;
            }
        }
        if let Some((item, _)) = folded.get(k) {
            out.push(clone_item(item));
        }
        k += 1;
    }
    out
}

fn para_left(i: &Item<'_>) -> Option<f64> {
    match i {
        Item::Para(b, _) => Some(b.rect[0]),
        _ => None,
    }
}

fn same_row(a: &Block, b: &Block) -> bool {
    let overlap = a.rect[3].min(b.rect[3]) - a.rect[1].max(b.rect[1]);
    overlap > 0.5 * (a.rect[3] - a.rect[1]).min(b.rect[3] - b.rect[1]).max(0.0)
}

fn clone_item<'a>(i: &Item<'a>) -> Item<'a> {
    match i {
        Item::Para(b, l) => Item::Para(b, *l),
        Item::List(l) => Item::List(*l),
        Item::Table(t) => Item::Table(t.clone()),
        Item::Img(im) => Item::Img(im),
        Item::PageBreak => Item::PageBreak,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_columns_read_column_by_column_under_a_heading() {
        // A heading across the page, two columns whose paragraph breaks line up, then a
        // footer across the page.
        let rects = [
            [72.0, 700.0, 540.0, 720.0],  // 0 heading
            [72.0, 600.0, 290.0, 690.0],  // 1 left, first
            [322.0, 600.0, 540.0, 690.0], // 2 right, first
            [72.0, 500.0, 290.0, 590.0],  // 3 left, second
            [322.0, 500.0, 540.0, 590.0], // 4 right, second
            [72.0, 60.0, 540.0, 72.0],    // 5 footer
        ];
        assert_eq!(reading_order(&rects), vec![0, 1, 3, 2, 4, 5]);
    }

    #[test]
    fn a_label_beside_its_value_is_not_two_columns() {
        let rects = [[72.0, 700.0, 110.0, 712.0], [130.0, 700.0, 400.0, 712.0], [72.0, 680.0, 110.0, 692.0], [130.0, 680.0, 400.0, 692.0]];
        assert_eq!(reading_order(&rects), vec![0, 1, 2, 3]);
        // A marker a little lower than its text on the same row still comes first.
        assert_eq!(reading_order(&[[90.0, 700.0, 400.0, 712.0], [72.0, 699.0, 80.0, 709.0]]), vec![1, 0]);
    }

    #[test]
    fn hostile_boxes_are_ordered_without_panicking() {
        let rects = vec![[f64::NAN, f64::INFINITY, -1.0, 5.0], [1e308, -1e308, 1e308, 1e308], [3.0, 3.0, 3.0, 3.0]];
        let mut order = reading_order(&rects);
        order.sort_unstable();
        assert_eq!(order, vec![0, 1, 2]);
        // Many boxes: every index exactly once, quickly.
        let many: Vec<[f64; 4]> = (0..30_000).map(|i| f64::from(i % 300)).map(|v| [v * 2.0, v, v * 2.0 + 1.0, v + 1.0]).collect();
        let started = std::time::Instant::now();
        let mut order = reading_order(&many);
        order.sort_unstable();
        assert_eq!(order, (0..30_000).collect::<Vec<_>>());
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        // A staircase that cuts deep stays bounded.
        let stairs: Vec<[f64; 4]> = (0..2000).map(f64::from).map(|v| [v * 30.0, v * 20.0, v * 30.0 + 25.0, v * 20.0 + 15.0]).collect();
        assert_eq!(reading_order(&stairs).len(), 2000);
    }

    #[test]
    fn markers_are_bullets_numbers_letters_and_roman_numerals() {
        let m = |t: &str| marker(t, false).map(|m| (m.numbering, m.delimiter, m.value, m.len));
        assert_eq!(m("• Apples"), Some((Numbering::Bullet, Delimiter::Period, 1, 4)));
        assert_eq!(m("12. Twelve"), Some((Numbering::Decimal, Delimiter::Period, 12, 4)));
        assert_eq!(m("3) Three"), Some((Numbering::Decimal, Delimiter::Paren, 3, 3)));
        assert_eq!(m("(4) Four"), Some((Numbering::Decimal, Delimiter::Parens, 4, 4)));
        assert_eq!(m("b. Bee"), Some((Numbering::LowerLetter, Delimiter::Period, 2, 3)));
        assert_eq!(m("C) Sea"), Some((Numbering::UpperLetter, Delimiter::Paren, 3, 3)));
        assert_eq!(m("iv. Four"), Some((Numbering::LowerRoman, Delimiter::Period, 4, 4)));
        assert_eq!(m("IX. Nine"), Some((Numbering::UpperRoman, Delimiter::Period, 9, 4)));
        for t in ["", "•", "1.", "1.5 metres", "2026. was a year", "Mr. Smith", "e.g. this", "iiii. no", "-5 degrees", "(a b) c", "1234. too long"]
        {
            assert_eq!(m(t), None, "{t:?}");
        }
        assert_eq!(marker("•", true).map(|m| m.numbering), Some(Numbering::Bullet));
        assert_eq!(marker("2.", true).map(|m| m.value), Some(2));
        assert_eq!(marker("2. x", true), None, "a lone marker has nothing after it");
    }
}
