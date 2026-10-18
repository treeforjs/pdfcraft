//! Recognize Text ▸ Editable Text and Images: the recognized words become real, visible text
//! in a matching standard font, and the page image they were read from stays as the background
//! with the text areas cleaned out.
//!
//! This module holds the pixel analysis and the PDF syntax; the engine renders the page, maps
//! pixels to user space and cleans the background image. Everything here is original code:
//!
//! - **Lines.** Words are grouped into lines by vertical overlap (at least half the smaller
//!   height), ordered left to right, and split at gaps wider than three line heights (columns).
//! - **Ink.** Within each line's box, Otsu's threshold on the gray histogram separates ink from
//!   paper. Lines without contrasting ink are not converted.
//! - **Baseline and size.** When none of the recognized letters descends, the baseline is the
//!   bottom of the ink; otherwise it is the end of the dense band of the ink row profile (rows
//!   with at least 30 % of the densest row's ink), since descenders are sparse. The size is the distance from
//!   the top of the ink to the baseline divided by the candidate font's ascender, cap or
//!   x-height (whichever the recognized text's tallest letters reach), so it is estimated per
//!   font candidate.
//! - **Font.** Three measures: the words' widths against the natural widths of their text in
//!   each standard face at that size; stroke contrast, the median horizontal ink run (vertical
//!   stems) over the median vertical run (horizontal strokes), high for serif faces and near 1
//!   for Helvetica and Courier; and stem weight (median stem over size) for bold. The face with
//!   the smallest combined cost wins; its width is then fitted exactly with `Tz` (horizontal
//!   scaling) and each word starts where it was read (`TJ` offsets; `Tc` stays 0).
//! - **Colour.** The mean colour of the darkest half of the ink; the paper colour is the median
//!   of the paper pixels around the line.
//! - **Suspects.** Words whose backend confidence is below `min_confidence`, or (when the
//!   backend reports none, as the bundled engine does) whose text fails the dictionary-free
//!   word-shape test of [`crate::orient::word_shaped`], are kept as image with invisible text
//!   over them, so a bad reading never replaces what was on the page. So are words in lines
//!   without measurable ink and words the standard fonts can't draw.

use crate::orient::Gray;
use crate::{OcrError, PlacedWord, Word};

/// At most this many text lines per page.
pub const MAX_LINES: usize = 200_000;
/// A line box larger than this many pixels is not analyzed (its words stay as image).
const MAX_LINE_PIXELS: u64 = 16_000_000;
/// Font sizes outside this range (points, or pixels in the analysis) are refused.
pub const MIN_SIZE: f64 = 0.5;
pub const MAX_SIZE: f64 = 5_000.0;
/// Horizontal scaling (percent) is fitted within this range.
pub const MIN_SCALE: f64 = 50.0;
pub const MAX_SCALE: f64 = 200.0;

/// A standard font family chosen for a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Family {
    Sans,
    Serif,
    Mono,
}

impl Family {
    pub const ALL: [Family; 3] = [Family::Sans, Family::Serif, Family::Mono];

    pub fn name(self) -> &'static str {
        match self {
            Family::Sans => "sans",
            Family::Serif => "serif",
            Family::Mono => "mono",
        }
    }

    /// Ascender (b, d, h, k, l), cap height and x-height in em units (the standard faces'
    /// published metrics).
    fn ascender(self) -> f64 {
        match self {
            Family::Sans => 0.718,
            Family::Serif => 0.683,
            Family::Mono => 0.629,
        }
    }
    fn cap(self) -> f64 {
        match self {
            Family::Sans => 0.718,
            Family::Serif => 0.662,
            Family::Mono => 0.562,
        }
    }
    fn x_height(self) -> f64 {
        match self {
            Family::Sans => 0.523,
            Family::Serif => 0.450,
            Family::Mono => 0.426,
        }
    }
    /// Expected stroke contrast (stem over horizontal stroke thickness).
    fn contrast(self) -> f64 {
        match self {
            Family::Sans => 1.2,
            Family::Serif => 2.4,
            Family::Mono => 1.0,
        }
    }
    /// Stems thicker than this fraction of the size are bold (midway between the faces'
    /// regular and bold stem widths).
    fn bold_stem(self) -> f64 {
        match self {
            Family::Sans => 0.114,
            Family::Serif => 0.111,
            Family::Mono => 0.078,
        }
    }
}

const SANS: Font = Font { family: Family::Sans, bold: false };

/// A standard font: a family, regular or bold. Drawn through the page resources PdfCraft adds
/// for its own text (WinAnsiEncoding).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Font {
    pub family: Family,
    pub bold: bool,
}

impl Font {
    pub fn base_font(self) -> &'static str {
        match (self.family, self.bold) {
            (Family::Sans, false) => "Helvetica",
            (Family::Sans, true) => "Helvetica-Bold",
            (Family::Serif, false) => "Times-Roman",
            (Family::Serif, true) => "Times-Bold",
            (Family::Mono, false) => "Courier",
            (Family::Mono, true) => "Courier-Bold",
        }
    }

    /// The page font resource name (`pdfcraft_edit::stamp` adds it).
    pub fn resource(self) -> &'static str {
        match (self.family, self.bold) {
            (Family::Sans, false) => "PCHelv",
            (Family::Sans, true) => "PCHelvB",
            (Family::Serif, false) => "PCTimes",
            (Family::Serif, true) => "PCTimesB",
            (Family::Mono, false) => "PCCour",
            (Family::Mono, true) => "PCCourB",
        }
    }

    /// Advance width of `text` in em units, `None` for characters the face has no width for.
    pub fn em_width(self, text: &str) -> Option<f64> {
        pdfcraft_fonts::standard_text_width(self.base_font(), text, 1.0)
    }
}

/// Why a word stays as image (with invisible text over it) instead of becoming visible text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SuspectReason {
    /// The backend's confidence is below `min_confidence`.
    LowConfidence,
    /// No backend confidence, and the text doesn't look like a word.
    Implausible,
    /// Its line has no measurable ink (or an unusable box).
    NoInk,
    /// The standard fonts can't draw a character in it.
    Unsupported,
}

impl SuspectReason {
    pub fn name(self) -> &'static str {
        match self {
            SuspectReason::LowConfidence => "low_confidence",
            SuspectReason::Implausible => "implausible",
            SuspectReason::NoInk => "no_ink",
            SuspectReason::Unsupported => "unsupported_characters",
        }
    }
}

/// A word kept as image.
#[derive(Clone, Debug, PartialEq)]
pub struct Suspect {
    /// Index into the page's recognized words (in recognition order).
    pub word: usize,
    pub confidence: Option<f32>,
    pub reason: SuspectReason,
}

/// One line as measured in the recognition image (pixels, y down).
#[derive(Clone, Debug, PartialEq)]
pub struct LineStyle {
    /// Indexes into the page's recognized words, left to right.
    pub words: Vec<usize>,
    /// Words of this line kept as image ([`Analysis::suspects`]), left to right.
    pub kept: Vec<usize>,
    pub font: Font,
    /// Font size (em) in pixels.
    pub size: f64,
    /// Baseline y.
    pub baseline: f64,
    /// The words' horizontal extent.
    pub left: f64,
    pub right: f64,
    /// The ink's vertical extent (descenders included).
    pub top: f64,
    pub bottom: f64,
    pub color: [u8; 3],
    pub paper: [u8; 3],
}

/// What [`analyze`] found on a page.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Analysis {
    pub lines: Vec<LineStyle>,
    pub suspects: Vec<Suspect>,
}

fn valid_rect(r: &[f32; 4]) -> bool {
    r.iter().all(|v| v.is_finite() && v.abs() < 1e7) && r[2] > r[0] && r[3] > r[1]
}

/// Whether `text` reads as a word, or is short punctuation (a dash, an ampersand).
fn plausible(text: &str) -> bool {
    let t = text.trim();
    crate::orient::word_shaped(t) || (t.chars().count() <= 3 && t.chars().all(|c| c.is_ascii_punctuation()))
}

/// Group word indexes (`candidates`, into `words`) into lines; see the module docs.
pub fn group_lines(words: &[Word], candidates: &[usize]) -> Vec<Vec<usize>> {
    let mut order: Vec<usize> = candidates.iter().copied().filter(|i| words.get(*i).is_some_and(|w| valid_rect(&w.rect))).collect();
    let rect = |i: usize| words.get(i).map_or([0.0; 4], |w| w.rect.map(f64::from));
    order.sort_by(|a, b| {
        let (ra, rb) = (rect(*a), rect(*b));
        (ra[1] + ra[3]).total_cmp(&(rb[1] + rb[3])).then(ra[0].total_cmp(&rb[0]))
    });
    // Each open line: its band (top, bottom) and its words.
    let mut lines: Vec<(f64, f64, Vec<usize>)> = Vec::new();
    for i in order {
        let r = rect(i);
        let h = r[3] - r[1];
        // Lines are created top to bottom: only the most recent few can still take a word.
        let start = lines.len().saturating_sub(8);
        let mut best: Option<(usize, f64)> = None;
        for (k, (t, b, _)) in lines.iter().enumerate().skip(start) {
            let overlap = r[3].min(*b) - r[1].max(*t);
            if overlap >= 0.5 * h.min(b - t) && best.is_none_or(|(_, o)| overlap > o) {
                best = Some((k, overlap));
            }
        }
        match best.and_then(|(k, _)| lines.get_mut(k)) {
            Some((t, b, ws)) => {
                *t = t.min(r[1]);
                *b = b.max(r[3]);
                ws.push(i);
            }
            None => lines.push((r[1], r[3], vec![i])),
        }
    }
    let mut out = Vec::new();
    for (_, _, mut ws) in lines {
        ws.sort_by(|a, b| rect(*a)[0].total_cmp(&rect(*b)[0]));
        let mut heights: Vec<f64> = ws.iter().map(|i| rect(*i)[3] - rect(*i)[1]).collect();
        heights.sort_by(f64::total_cmp);
        let height = heights.get(heights.len() / 2).copied().unwrap_or(0.0);
        let mut current: Vec<usize> = Vec::new();
        let mut right = f64::NEG_INFINITY;
        for i in ws {
            let r = rect(i);
            if !current.is_empty() && r[0] - right > 3.0 * height {
                out.push(std::mem::take(&mut current));
            }
            right = right.max(r[2]);
            current.push(i);
        }
        if !current.is_empty() {
            out.push(current);
        }
        if out.len() >= MAX_LINES {
            out.truncate(MAX_LINES);
            break;
        }
    }
    out
}

/// Otsu's threshold over a histogram: the last ink level. `None` without two classes.
fn otsu(hist: &[u64; 256]) -> Option<u8> {
    let total: u64 = hist.iter().sum();
    if total == 0 {
        return None;
    }
    let sum: f64 = hist.iter().enumerate().map(|(v, c)| v as f64 * *c as f64).sum();
    let (mut w0, mut sum0, mut best, mut at) = (0.0f64, 0.0f64, -1.0f64, None);
    for (v, c) in hist.iter().enumerate() {
        w0 += *c as f64;
        sum0 += v as f64 * *c as f64;
        let w1 = total as f64 - w0;
        if w0 <= 0.0 || w1 <= 0.0 {
            continue;
        }
        let (m0, m1) = (sum0 / w0, (sum - sum0) / w1);
        let between = w0 * w1 * (m0 - m1) * (m0 - m1);
        if between > best {
            best = between;
            at = u8::try_from(v).ok();
        }
    }
    at
}

fn median_of(hist: &[u64]) -> Option<usize> {
    let total: u64 = hist.iter().sum();
    if total == 0 {
        return None;
    }
    let mut seen = 0;
    for (v, c) in hist.iter().enumerate() {
        seen += c;
        if seen * 2 >= total {
            return Some(v);
        }
    }
    None
}

/// Which height the text's tallest letters reach, for a family: ascender, cap or x-height.
fn top_ratio(text: &str, family: Family) -> f64 {
    if text.chars().any(|c| "bdfhklij!?".contains(c)) {
        family.ascender()
    } else if text.chars().any(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) {
        family.cap()
    } else if text.chars().any(|c| c.is_ascii_lowercase()) {
        family.x_height()
    } else {
        family.cap()
    }
}

/// Integer pixel bounds `[x0, y0, x1, y1)` of a box grown by `pad`, clamped to the image.
fn bounds(gray: &Gray, r: [f64; 4], pad: f64) -> Option<[usize; 4]> {
    let (w, h) = (f64::from(gray.width), f64::from(gray.height));
    let x0 = (r[0] - pad).floor().clamp(0.0, w);
    let y0 = (r[1] - pad).floor().clamp(0.0, h);
    let x1 = (r[2] + pad).ceil().clamp(0.0, w);
    let y1 = (r[3] + pad).ceil().clamp(0.0, h);
    if !(x1 > x0 && y1 > y0) || (x1 - x0) * (y1 - y0) > MAX_LINE_PIXELS as f64 {
        return None;
    }
    Some([x0 as usize, y0 as usize, x1 as usize, y1 as usize])
}

fn at(gray: &Gray, x: usize, y: usize) -> u8 {
    y.checked_mul(gray.width as usize).and_then(|i| i.checked_add(x)).and_then(|i| gray.pixels.get(i)).copied().unwrap_or(255)
}

/// Measure one line (word indexes left to right) and choose its font.
/// Measure one line and choose its font from the words `measured` (left to right; the line's
/// other words are kept as image and only bound the area looked at).
fn analyze_line(gray: &Gray, rgb: &dyn Fn(f64, f64) -> Option<[u8; 3]>, words: &[Word], measured: &[usize]) -> Option<LineStyle> {
    let rects: Vec<[f64; 4]> = measured.iter().filter_map(|i| words.get(*i)).map(|w| w.rect.map(f64::from)).collect();
    let l = rects.iter().map(|r| r[0]).fold(f64::INFINITY, f64::min);
    let t = rects.iter().map(|r| r[1]).fold(f64::INFINITY, f64::min);
    let r = rects.iter().map(|r| r[2]).fold(f64::NEG_INFINITY, f64::max);
    let b = rects.iter().map(|r| r[3]).fold(f64::NEG_INFINITY, f64::max);
    let height = b - t;
    if !(height.is_finite() && height > 0.0 && r > l) {
        return None;
    }
    // Threshold and paper level from the box with some paper around it.
    let outer = bounds(gray, [l, t, r, b], (0.25 * height).max(2.0))?;
    let mut hist = [0u64; 256];
    for y in outer[1]..outer[3] {
        for x in outer[0]..outer[2] {
            if let Some(c) = hist.get_mut(usize::from(at(gray, x, y))) {
                *c += 1;
            }
        }
    }
    let threshold = otsu(&hist)?;
    let darkest = hist.iter().position(|c| *c > 0)?;
    let paper_level = median_of(hist.get(usize::from(threshold) + 1..)?)?.saturating_add(usize::from(threshold) + 1);
    if paper_level.saturating_sub(darkest) < 48 {
        return None;
    }
    // The ink of this line: the measured words' boxes, grown a little for anti-aliased edges
    // (words kept as image between them don't count).
    let grow = (0.1 * height).max(1.0);
    let inner = bounds(gray, [l, t, r, b], grow)?;
    let columns: Vec<bool> = (inner[0]..inner[2]).map(|x| rects.iter().any(|r| (r[0] - grow..r[2] + grow).contains(&(x as f64 + 0.5)))).collect();
    let ink = |x: usize, y: usize| columns.get(x.wrapping_sub(inner[0])).copied().unwrap_or(false) && at(gray, x, y) <= threshold;
    let rows: Vec<u64> = (inner[1]..inner[3]).map(|y| (inner[0]..inner[2]).filter(|x| ink(*x, y)).count() as u64).collect();
    let first = rows.iter().position(|c| *c > 0)?;
    let last = rows.iter().rposition(|c| *c > 0)?;
    // The baseline: the bottom of the ink when no recognized letter descends; otherwise the
    // end of the dense band (rows with at least 30 % of the densest row's ink, allowing short
    // gaps) that holds the densest row, since descenders are sparse.
    let descends = measured.iter().filter_map(|i| words.get(*i)).any(|w| w.text.chars().any(|c| "gjpqy,;".contains(c)));
    let mut base = last;
    if descends {
        let peak = rows.iter().copied().max().unwrap_or(0);
        let densest = rows.iter().position(|c| *c == peak).unwrap_or(first);
        let dense = |c: u64| c.saturating_mul(10) >= peak.saturating_mul(3);
        let tolerance = 2 + (last - first) / 20;
        let (mut gap, mut end) = (0usize, densest);
        for (y, c) in rows.iter().enumerate().take(last + 1).skip(densest + 1) {
            if dense(*c) {
                end = y;
                gap = 0;
            } else {
                gap += 1;
                if gap > tolerance {
                    break;
                }
            }
        }
        base = end;
    }
    let top = (inner[1] + first) as f64;
    let bottom = (inner[1] + last + 1) as f64;
    let baseline = (inner[1] + base + 1) as f64;
    let rise = baseline - top;
    if rise < 2.0 {
        return None;
    }
    // Stroke runs between the top of the ink and the baseline.
    let limit = (0.45 * rise).max(2.0) as usize;
    let mut horizontal = vec![0u64; limit + 1];
    let mut vertical = vec![0u64; limit + 1];
    let (y0, y1) = (inner[1] + first, inner[1] + base + 1);
    for y in y0..y1 {
        let mut run = 0usize;
        for x in inner[0]..=inner[2] {
            if x < inner[2] && ink(x, y) {
                run += 1;
            } else if run > 0 {
                if let Some(c) = horizontal.get_mut(run) {
                    *c += 1;
                }
                run = 0;
            }
        }
    }
    for x in inner[0]..inner[2] {
        let mut run = 0usize;
        for y in y0..=y1 {
            if y < y1 && ink(x, y) {
                run += 1;
            } else if run > 0 {
                if let Some(c) = vertical.get_mut(run) {
                    *c += 1;
                }
                run = 0;
            }
        }
    }
    let enough = |h: &[u64]| h.iter().sum::<u64>() >= 8;
    let stem = enough(&horizontal).then(|| median_of(&horizontal)).flatten().map(|v| v as f64);
    let thin = enough(&vertical).then(|| median_of(&vertical)).flatten().map(|v| v as f64);
    let contrast = stem.zip(thin).filter(|(_, v)| *v > 0.0).map(|(h, v)| h / v);

    let texts: Vec<&str> = measured.iter().filter_map(|i| words.get(*i)).map(|w| w.text.trim()).collect();
    let text = texts.join(" ");
    // The words' own widths against their natural widths: gaps between words don't count.
    let width: f64 = rects.iter().map(|r| r[2] - r[0]).sum();
    let mut best: Option<(f64, Font, f64)> = None;
    for family in Family::ALL {
        let size = rise / top_ratio(&text, family);
        let bold = stem.is_some_and(|s| s / size > family.bold_stem());
        let font = Font { family, bold };
        let Some(natural) = texts.iter().map(|t| font.em_width(t)).sum::<Option<f64>>().map(|w| w * size).filter(|w| *w > 0.0) else { continue };
        let cost = 2.0 * (width / natural).ln().abs() + contrast.map_or(0.0, |c| (c / family.contrast()).ln().abs());
        if cost.is_finite() && best.is_none_or(|(c, ..)| cost < c) {
            best = Some((cost, font, size));
        }
    }
    let (_, font, size) = best?;
    if !(MIN_SIZE..=MAX_SIZE).contains(&size) {
        return None;
    }

    // Colour: the mean of the darkest half of the ink. Paper: the median around the line.
    let core = darkest + (usize::from(threshold).saturating_sub(darkest)) / 2;
    let (mut sum, mut n) = ([0u64; 3], 0u64);
    let mut paper: [Vec<u8>; 3] = Default::default();
    let area = (outer[2] - outer[0]).saturating_mul(outer[3] - outer[1]);
    let stride = (area / 4096).max(1);
    let mut k = 0usize;
    for y in outer[1]..outer[3] {
        for x in outer[0]..outer[2] {
            let v = usize::from(at(gray, x, y));
            let inside = (inner[0]..inner[2]).contains(&x) && (inner[1]..inner[3]).contains(&y);
            if inside && v <= core {
                let c = rgb(x as f64 + 0.5, y as f64 + 0.5).unwrap_or([v as u8; 3]);
                for (s, c) in sum.iter_mut().zip(c) {
                    *s += u64::from(c);
                }
                n += 1;
            } else if v > usize::from(threshold) {
                k += 1;
                if k.is_multiple_of(stride) {
                    let c = rgb(x as f64 + 0.5, y as f64 + 0.5).unwrap_or([v as u8; 3]);
                    for (p, c) in paper.iter_mut().zip(c) {
                        p.push(c);
                    }
                }
            }
        }
    }
    let color = if n == 0 { [darkest as u8; 3] } else { sum.map(|s| (s / n).min(255) as u8) };
    let paper = paper.map(|mut p| {
        p.sort_unstable();
        p.get(p.len() / 2).copied().unwrap_or(paper_level.min(255) as u8)
    });
    Some(LineStyle { words: measured.to_vec(), kept: Vec::new(), font, size, baseline, left: l, right: r, top, bottom, color, paper })
}

/// Measure the recognized `words` of a page (in recognition order) in `gray`, the image they
/// were read from; `rgb` gives the page's colour at a point of that image. Words below
/// `min_confidence` (0–1; see the module docs) are reported as suspects instead of lines.
pub fn analyze(gray: &Gray, rgb: &dyn Fn(f64, f64) -> Option<[u8; 3]>, words: &[Word], min_confidence: f32) -> Analysis {
    let mut out = Analysis::default();
    // Suspects still take part in grouping, so a line isn't split where one was.
    let mut candidates = Vec::new();
    let mut suspect = vec![false; words.len()];
    for (i, w) in words.iter().enumerate() {
        if w.text.trim().is_empty() {
            continue;
        }
        let reason = if crate::validate_text(&w.text).is_err() || SANS.em_width(w.text.trim()).is_none() {
            Some(SuspectReason::Unsupported)
        } else if !valid_rect(&w.rect) {
            Some(SuspectReason::NoInk)
        } else {
            match w.confidence {
                Some(c) if !c.is_finite() || c < min_confidence => Some(SuspectReason::LowConfidence),
                None if min_confidence > 0.0 && !plausible(&w.text) => Some(SuspectReason::Implausible),
                _ => None,
            }
        };
        if let Some(reason) = reason {
            out.suspects.push(Suspect { word: i, confidence: w.confidence, reason });
            if let Some(s) = suspect.get_mut(i) {
                *s = true;
            }
        }
        if valid_rect(&w.rect) {
            candidates.push(i);
        }
    }
    // Pixels analyzed over the page are bounded (hostile overlapping boxes).
    let budget = u64::from(gray.width).saturating_mul(u64::from(gray.height)).saturating_mul(8).saturating_add(1 << 20);
    let mut spent = 0u64;
    for line in group_lines(words, &candidates) {
        let (kept, line): (Vec<usize>, Vec<usize>) = line.into_iter().partition(|i| suspect.get(*i).copied().unwrap_or(true));
        if line.is_empty() {
            continue;
        }
        let area = line
            .iter()
            .filter_map(|i| words.get(*i))
            .map(|w| f64::from(w.rect[2] - w.rect[0]).max(0.0) * f64::from(w.rect[3] - w.rect[1]).max(0.0) * 4.0)
            .sum::<f64>()
            .min(u64::MAX as f64) as u64;
        spent = spent.saturating_add(area);
        let style = if spent <= budget { analyze_line(gray, rgb, words, &line) } else { None };
        match style {
            Some(s) => out.lines.push(LineStyle { kept, ..s }),
            None => out.suspects.extend(line.iter().map(|i| Suspect {
                word: *i,
                confidence: words.get(*i).and_then(|w| w.confidence),
                reason: SuspectReason::NoInk,
            })),
        }
    }
    out.suspects.sort_by_key(|s| s.word);
    out
}

/// One word of a visible line: its text and where it starts along the line (user units).
#[derive(Clone, Debug, PartialEq)]
pub struct LineWord {
    pub text: String,
    pub offset: f64,
    /// Kept as image: drawn invisibly (`3 Tr`) in its place in the line, so the line's text
    /// reads in order.
    pub hidden: bool,
    /// The length it was read over (user units); the line's horizontal scaling fits these.
    pub width: f64,
}

/// A visible line of text in user space: `origin` is the start of its baseline, `direction`
/// and `up` unit vectors along and across it.
#[derive(Clone, Debug, PartialEq)]
pub struct TextLine {
    pub font: Font,
    pub size: f64,
    pub origin: [f64; 2],
    pub direction: [f64; 2],
    pub up: [f64; 2],
    pub color: [u8; 3],
    pub words: Vec<LineWord>,
}

/// An area of the page image to clean (a word's box in user space, corners in order) and the
/// paper colour around it.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    pub quad: [[f64; 2]; 4],
    pub paper: [u8; 3],
}

impl TextLine {
    /// The words joined as they are drawn.
    pub fn text(&self) -> String {
        self.words.iter().map(|w| w.text.trim()).collect::<Vec<_>>().join(" ")
    }

    /// The horizontal scaling (percent) that fits the line's words to the widths they were
    /// read over.
    pub fn horizontal_scale(&self) -> Option<f64> {
        horizontal_scale(self.font, self.words.iter().map(|w| (w.text.as_str(), w.width)), self.size)
    }
}

/// `Tz` (percent) that makes words (text, width read over) in `font` at `size` as wide as they
/// were read, together, within [`MIN_SCALE`]..[`MAX_SCALE`]. The spaces between them don't
/// count: each word is positioned where it was read.
pub fn horizontal_scale<'a>(font: Font, words: impl IntoIterator<Item = (&'a str, f64)>, size: f64) -> Option<f64> {
    let (mut natural, mut width) = (0.0, 0.0);
    for (text, w) in words {
        natural += font.em_width(text.trim())? * size;
        width += w;
    }
    (natural > 0.0 && width.is_finite() && width > 0.0).then(|| (100.0 * width / natural).clamp(MIN_SCALE, MAX_SCALE))
}

fn finite(v: &[f64]) -> bool {
    v.iter().all(|v| v.is_finite() && v.abs() <= 10_000_000.0)
}

/// Validate visible lines and regions before producing PDF syntax or making any edit.
pub fn validate(lines: &[TextLine], regions: &[Region]) -> Result<(), OcrError> {
    let bad = |m: &str| Err(OcrError::InvalidText(m.into()));
    if lines.len() > MAX_LINES || regions.len() > 200_000 {
        return bad("at most 200000 lines and word regions per page are supported");
    }
    let mut bytes = 0usize;
    let mut words = 0usize;
    for line in lines {
        if line.words.is_empty() {
            return bad("a text line needs at least one word");
        }
        words = words.saturating_add(line.words.len());
        if words > 200_000 {
            return bad("at most 200000 words per page are supported");
        }
        if !(line.size.is_finite() && (MIN_SIZE..=MAX_SIZE).contains(&line.size)) {
            return bad("text sizes must be between 0.5 and 5000 points");
        }
        let unit = |v: [f64; 2]| (v[0].hypot(v[1]) - 1.0).abs() < 1e-3;
        if !finite(&line.origin) || !finite(&line.direction) || !finite(&line.up) || !unit(line.direction) || !unit(line.up) {
            return bad("text lines need a finite origin and unit direction vectors");
        }
        if (line.direction[0] * line.up[1] - line.direction[1] * line.up[0]).abs() < 0.05 {
            return bad("a text line's direction and up vectors must not be parallel");
        }
        for w in &line.words {
            crate::validate_text(&w.text)?;
            if w.text.trim().is_empty() {
                return bad("visible words must not be empty");
            }
            if line.font.em_width(w.text.trim()).is_none() {
                return bad("a visible word has a character the standard fonts can't draw");
            }
            if !finite(&[w.offset, w.width]) || w.width < 0.0 {
                return bad("word offsets and widths must be finite");
            }
            bytes = bytes.saturating_add(w.text.len());
            if bytes > 16 * 1024 * 1024 {
                return bad("a page's recognized text must be at most 16 MiB");
            }
        }
    }
    if regions.iter().any(|r| !finite(r.quad.as_flattened())) {
        return bad("word regions must have finite coordinates within ten million points");
    }
    Ok(())
}

fn num(v: f64) -> String {
    let s = format!("{:.4}", if v.abs() < 1e-9 { 0.0 } else { v });
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

fn rgb(c: [u8; 3]) -> String {
    let [r, g, b] = c.map(|v| num(f64::from(v) / 255.0));
    format!("{r} {g} {b}")
}

/// Page content for Editable Text and Images: `covers` filled with their paper colour (areas
/// the background image could not be cleaned in), then each line as visible text in its font,
/// size and colour, fitted with `Tz` and positioned word by word with `TJ`, then `hidden` as
/// invisible searchable text ([`crate::text_layer`]) over the words kept as image.
pub fn content(lines: &[TextLine], covers: &[Region], hidden: &[PlacedWord]) -> Result<Vec<u8>, OcrError> {
    validate(lines, covers)?;
    crate::validate_words(hidden)?;
    let mut out = b"q\n".to_vec();
    for c in covers {
        let [a, b, cc, d] = c.quad;
        out.extend_from_slice(
            format!(
                "{} rg\n{} {} m {} {} l {} {} l {} {} l h f\n",
                rgb(c.paper),
                num(a[0]),
                num(a[1]),
                num(b[0]),
                num(b[1]),
                num(cc[0]),
                num(cc[1]),
                num(d[0]),
                num(d[1])
            )
            .as_bytes(),
        );
    }
    for line in lines {
        let scale = line.horizontal_scale().ok_or_else(|| OcrError::InvalidText("a text line could not be measured".into()))?;
        let th = scale / 100.0;
        let unit = line.size * th;
        let [dx, dy] = line.direction;
        let [ux, uy] = line.up;
        out.extend_from_slice(
            format!(
                "BT\n/{} {} Tf\n0 Tr 0 Tc 0 Tw {} Tz\n{} rg\n{} {} {} {} {} {} Tm\n[",
                line.font.resource(),
                num(line.size),
                num(scale),
                rgb(line.color),
                num(dx),
                num(dy),
                num(ux),
                num(uy),
                num(line.origin[0]),
                num(line.origin[1])
            )
            .as_bytes(),
        );
        let mut pen = 0.0f64;
        let count = line.words.len();
        let mut invisible = false;
        for (i, w) in line.words.iter().enumerate() {
            if w.hidden != invisible {
                // The text position carries over from one TJ to the next within BT.
                invisible = w.hidden;
                out.extend_from_slice(if invisible { b"] TJ\n3 Tr\n[" } else { b"] TJ\n0 Tr\n[" });
            }
            if i > 0 {
                // Move from where the previous word (and its space) ended to where this one
                // was read; TJ numbers are thousandths of the scaled text space, subtracted.
                let shift = -(w.offset - pen) / unit * 1000.0;
                if shift.abs() >= 0.5 && shift.is_finite() {
                    out.extend_from_slice(format!("{} ", num(shift.clamp(-1e7, 1e7))).as_bytes());
                }
                pen = w.offset;
            }
            let mut text = w.text.trim().to_owned();
            if i + 1 < count {
                text.push(' ');
            }
            pen += line.font.em_width(&text).unwrap_or(0.0) * unit;
            out.extend(pdfcraft_fonts::literal(&pdfcraft_fonts::win_ansi(&text)));
        }
        out.extend_from_slice(b"] TJ\nET\n");
    }
    out.extend_from_slice(b"Q\n");
    if !hidden.is_empty() {
        out.extend(crate::text_layer(hidden)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, rect: [f32; 4]) -> Word {
        Word { text: text.into(), rect, confidence: None }
    }

    #[test]
    fn words_are_grouped_into_lines_in_reading_order_and_split_at_column_gaps() {
        let words = [
            word("second", [60.0, 10.0, 100.0, 30.0]),
            word("below", [10.0, 50.0, 50.0, 70.0]),
            word("first", [10.0, 12.0, 50.0, 31.0]),
            word("far", [400.0, 11.0, 440.0, 30.0]),
        ];
        assert_eq!(group_lines(&words, &[0, 1, 2, 3]), vec![vec![2, 0], vec![3], vec![1]]);
        assert!(group_lines(&words, &[]).is_empty());
        // Hostile boxes are ignored, not grouped.
        let bad = [word("x", [f32::NAN, 0.0, 1.0, 1.0]), word("y", [5.0, 5.0, 1.0, 1.0])];
        assert!(group_lines(&bad, &[0, 1, 7]).is_empty());
    }

    #[test]
    fn otsu_splits_ink_from_paper() {
        let mut hist = [0u64; 256];
        hist[20] = 100;
        hist[230] = 900;
        let t = otsu(&hist).unwrap();
        assert!((20..230).contains(&t));
        assert_eq!(otsu(&[0; 256]), None);
    }

    /// A synthetic line: black stems of `stem` px and horizontal bars of `bar` px on white.
    fn glyphs(stem: usize, bar: usize) -> Gray {
        let (w, h) = (400u32, 100u32);
        let mut pixels = vec![250u8; (w * h) as usize];
        for g in 0..12 {
            let x0 = 20 + g * 30;
            for y in 30..70 {
                for x in x0..x0 + stem {
                    pixels[y * w as usize + x] = 15;
                }
            }
            for y in 30..30 + bar {
                for x in x0..x0 + 20 {
                    pixels[y * w as usize + x] = 15;
                }
            }
        }
        Gray { width: w, height: h, pixels }
    }

    #[test]
    fn stroke_contrast_tells_serif_from_sans() {
        // About as wide as this text is in Helvetica or Times at the measured size.
        let words = [word("Hold the line", [18.0, 28.0, 382.0, 72.0])];
        let none = |_: f64, _: f64| None;
        let sans = analyze_line(&glyphs(4, 4), &none, &words, &[0]).unwrap();
        let serif = analyze_line(&glyphs(6, 2), &none, &words, &[0]).unwrap();
        assert_eq!(sans.font.family, Family::Sans, "{sans:?}");
        assert_eq!(serif.font.family, Family::Serif, "{serif:?}");
        assert!((sans.baseline - 70.0).abs() <= 1.0 && sans.top == 30.0, "{sans:?}");
        assert!(sans.color[0] < 40 && sans.paper[0] > 240, "{sans:?}");
    }

    #[test]
    fn suspects_are_low_confidence_implausible_unsupported_or_inkless() {
        let gray = Gray { width: 50, height: 50, pixels: vec![255; 2500] };
        let words = [
            Word { text: "sure".into(), rect: [1.0, 1.0, 20.0, 10.0], confidence: Some(0.2) },
            word("xQzzkW", [1.0, 20.0, 20.0, 30.0]),
            word("漢", [1.0, 30.0, 20.0, 40.0]),
            word("blank", [25.0, 1.0, 45.0, 10.0]),
            word("  ", [25.0, 20.0, 45.0, 30.0]),
        ];
        let a = analyze(&gray, &|_, _| None, &words, 0.5);
        let reasons: Vec<_> = a.suspects.iter().map(|s| (s.word, s.reason)).collect();
        assert_eq!(
            reasons,
            vec![(0, SuspectReason::LowConfidence), (1, SuspectReason::Implausible), (2, SuspectReason::Unsupported), (3, SuspectReason::NoInk)]
        );
        assert!(a.lines.is_empty());
        // min_confidence 0 trusts every reading the fonts can draw.
        let a = analyze(&gray, &|_, _| None, &words, 0.0);
        assert!(a.suspects.iter().all(|s| s.reason != SuspectReason::LowConfidence && s.reason != SuspectReason::Implausible));
    }

    #[test]
    fn hostile_words_and_images_do_not_panic() {
        let gray = Gray { width: 3, height: 2, pixels: vec![0, 255, 0] };
        let words = [
            word("a", [f32::INFINITY, 0.0, 1.0, 1.0]),
            word("b", [-1e9, -1e9, 1e9, 1e9]),
            word("c", [0.0, 0.0, 1e30, 1e30]),
            word("d", [0.0, 0.0, 3.0, 2.0]),
            Word { text: "e".into(), rect: [0.0, 0.0, 3.0, 2.0], confidence: Some(f32::NAN) },
        ];
        let a = analyze(&gray, &|_, _| Some([1, 2, 3]), &words, 0.5);
        assert_eq!(a.lines.iter().map(|l| l.words.len()).sum::<usize>() + a.suspects.len(), words.len());
        let empty = Gray { width: 0, height: 0, pixels: Vec::new() };
        let _ = analyze(&empty, &|_, _| None, &words, 0.5);
    }

    fn line() -> TextLine {
        TextLine {
            font: Font { family: Family::Serif, bold: false },
            size: 12.0,
            origin: [72.0, 700.0],
            direction: [1.0, 0.0],
            up: [0.0, 1.0],
            color: [10, 20, 30],
            words: vec![
                LineWord { text: "Hello".into(), offset: 0.0, width: 28.0, hidden: false },
                LineWord { text: "world".into(), offset: 34.0, width: 28.0, hidden: false },
            ],
        }
    }

    #[test]
    fn visible_lines_are_fitted_text_runs() {
        let s = String::from_utf8(content(&[line()], &[], &[]).unwrap()).unwrap();
        let scale = line().horizontal_scale().unwrap();
        assert!(s.contains(&format!("/PCTimes 12 Tf\n0 Tr 0 Tc 0 Tw {} Tz", num(scale))), "{s}");
        assert!(s.contains("1 0 0 1 72 700 Tm") && s.contains("[(Hello )-") && s.contains("(world)] TJ"), "{s}");
        assert!(!s.contains("3 Tr"), "{s}");
        let cover = Region { quad: [[0.0, 0.0], [10.0, 0.0], [10.0, 5.0], [0.0, 5.0]], paper: [255, 255, 255] };
        let hidden = PlacedWord { text: "maybe".into(), origin: [1.0, 1.0], across: [10.0, 0.0], up: [0.0, 5.0] };
        let s = String::from_utf8(content(&[line()], std::slice::from_ref(&cover), &[hidden]).unwrap()).unwrap();
        assert!(s.contains("1 1 1 rg\n0 0 m 10 0 l 10 5 l 0 5 l h f"), "{s}");
        assert!(s.ends_with("ET\nEMC\nQ\n") && s.contains("3 Tr"), "{s}");
        // A word kept as image in the middle of a line is drawn invisibly in its place.
        let mut mixed = line();
        mixed.words.insert(1, LineWord { text: "maybe".into(), offset: 20.0, width: 10.0, hidden: true });
        mixed.words[2].offset = 40.0;
        let s = String::from_utf8(content(&[mixed], &[], &[]).unwrap()).unwrap();
        assert!(s.contains("(Hello )] TJ\n3 Tr\n[") && s.contains("(maybe )] TJ\n0 Tr\n[") && s.contains("(world)] TJ\nET"), "{s}");
    }

    #[test]
    fn bad_lines_are_refused_before_writing() {
        let cases: Vec<TextLine> = vec![
            TextLine { size: f64::NAN, ..line() },
            TextLine { size: 0.0, ..line() },
            TextLine { direction: [0.0, 0.0], ..line() },
            TextLine { up: [1.0, 0.0], ..line() },
            TextLine { origin: [f64::INFINITY, 0.0], ..line() },
            TextLine { words: vec![LineWord { text: "x".into(), offset: 0.0, width: -1.0, hidden: false }], ..line() },
            TextLine { words: Vec::new(), ..line() },
            TextLine { words: vec![LineWord { text: "漢字".into(), offset: 0.0, width: 1.0, hidden: false }], ..line() },
            TextLine { words: vec![LineWord { text: " ".into(), offset: 0.0, width: 1.0, hidden: false }], ..line() },
            TextLine { words: vec![LineWord { text: "x".into(), offset: f64::NAN, width: 1.0, hidden: false }], ..line() },
        ];
        for bad in cases {
            assert!(content(std::slice::from_ref(&bad), &[], &[]).is_err(), "{bad:?}");
        }
        let cover = Region { quad: [[f64::NAN, 0.0]; 4], paper: [0; 3] };
        assert!(content(&[], &[cover], &[]).is_err());
    }
}
