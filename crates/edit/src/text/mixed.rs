//! Run-preserving paragraph reflow, used for every upright paragraph edit (whole-paragraph
//! replacement as well as selected ranges). Character offsets refer to Unicode scalars; PDF
//! character codes are recovered through each run's own encoding, including multi-scalar
//! ToUnicode maps. Untouched characters keep their source codes and TJ adjustments, and
//! repeated spaces are shown as typed.
//!
//! Emission is compact: consecutive glyphs of one face on one row become a single `Tj`/`TJ`
//! placed by one `Tm`, with the source adjustments (kerning, positioned gaps) and any
//! justification written back as TJ numbers. A face's text state is replayed only when the
//! face changes. A code-less logical space (a gap the source made by repositioning) starts a
//! new placement instead, and each underlined glyph keeps its own placement, marker and path.

use super::*;

pub(super) const MAX_SEGMENTS: usize = 8192;
const MAX_EDIT_CHARACTERS: usize = 1_000_000;
/// Underlined glyphs are each placed, marked and given their own path, so the character ceiling
/// alone would allow tens of millions of in-memory operators. The emitted count is budgeted
/// before anything is emitted: a guaranteed minimum while the glyphs are prepared, then the
/// exact count of the planned placements. About 150 bytes per operator keeps the worst case to a
/// few hundred megabytes; ordinary paragraphs use a tiny fraction of it.
pub(super) const MAX_EMITTED_OPS: usize = 2_000_000;
/// One placement of consecutive glyphs: `Tm` and `Tj`/`TJ`.
const CHUNK_OPS: usize = 2;
/// An underlined glyph: `Tm`, its text marker pair around `Tj`, and its marked path
/// (`BDC q cm RG w J d m l S Q EMC`).
const UNDERLINED_GLYPH_OPS: usize = 4 + 12;
/// Paragraph framing: `q`, an optional `cm`, `BT`, `ET` and the closing `Q`.
const FRAME_OPS: usize = 5;
/// Slack (user space) when fitting a row, far below anything visible.
const WIDTH_TOLERANCE: f64 = 0.01;

/// Characters a single PDF paragraph cannot represent (line and page breaks, tabs) are shown as
/// one space each; a CR LF pair is one break. Everything else, including repeated, no-break and
/// ideographic spaces, is shown as typed.
pub(super) fn typed_char(ch: char) -> char {
    if matches!(ch, '\n' | '\r' | '\t' | '\u{b}' | '\u{c}' | '\u{85}' | '\u{2028}' | '\u{2029}') { ' ' } else { ch }
}

/// Replacement text for a whole paragraph as it will be shown (see [`typed_char`]).
pub(super) fn typed_text(text: &str) -> String {
    text.replace("\r\n", "\n").chars().map(typed_char).collect()
}

/// Largest cosine between a paragraph's text axes in user space still treated as a right angle
/// (about 0.006°: under a hundredth of a point of skew across a 100 pt line).
const SKEW_TOLERANCE: f64 = 1e-4;

/// Whether the run path can place a paragraph: its text space, in user space, is upright text
/// turned by any angle (through `Tm`, the CTM or both), with its own horizontal and vertical
/// scales along its axes. Grouping, gap advances, row baselines and widths are all measured in
/// the paragraph's text [`Frame`] (the inverse of its normalized text axes), and glyphs are
/// placed in text space relative to the first line's text matrix, so turning the paragraph
/// changes nothing but that frame. Skewed and reflected text space is not placed yet.
pub(super) fn supported_frame(lines: &[TextLine], block: &TextBlock) -> bool {
    let Some(first) = block.lines.first().and_then(|i| lines.get(*i)) else { return false };
    let [a, b, c, d, _, _] = Matrix(first.origin.tm).then(&Matrix(first.origin.ctm)).0;
    let (horizontal, vertical) = (a.hypot(b), c.hypot(d));
    let skew = (a * c + b * d) / (horizontal * vertical);
    [a, b, c, d, horizontal, vertical, skew].into_iter().all(f64::is_finite)
        && horizontal > 1e-9
        && vertical > 1e-9
        && skew.abs() <= SKEW_TOLERANCE
        && a * d - b * c > 0.0
        // Every line is measured in the first line's frame.
        && block.lines.iter().filter_map(|i| lines.get(*i)).all(|l| l.origin.ctm == first.origin.ctm && l.origin.tm.get(..4) == first.origin.tm.get(..4))
        && block.runs.iter().all(|r| r.state.scale.is_finite() && r.state.scale > 0.0)
}

/// A text or path matrix as operands: its linear part exactly as the source has it (rounding a
/// turned paragraph's axes would turn and scale every glyph slightly), its translation to a
/// ten-thousandth of a unit like other emitted numbers.
fn matrix_operands(m: [f64; 6]) -> Vec<Object> {
    m.into_iter().enumerate().map(|(i, v)| if i < 4 && v.fract() != 0.0 { Object::Real(v) } else { pdfcraft_content::num(v) }).collect()
}

fn over_budget() -> EditError {
    EditError::Invalid(format!(
        "this paragraph edit would emit more than {MAX_EMITTED_OPS} PDF operators (each character is placed individually and \
         underlined characters add a marked path); split the text into smaller paragraphs or remove underlines from part of it"
    ))
}

pub(super) fn validate_text(text: &str) -> Result<(), EditError> {
    if text.chars().take(MAX_EDIT_CHARACTERS + 1).count() > MAX_EDIT_CHARACTERS {
        return Err(EditError::Invalid("edit paragraphs with at most 1,000,000 characters".into()));
    }
    Ok(())
}

struct Segment {
    text: String,
    source: usize,
    selected: bool,
    source_range: Option<std::ops::Range<usize>>,
}

struct Face {
    state: TextState,
    metrics: Rc<Metrics>,
    underline: bool,
}

struct Prepared {
    faces: Vec<Face>,
    glyphs: Vec<Glyph>,
    substituted: Option<String>,
}

struct Glyph {
    bytes: Vec<u8>,
    face: usize,
    /// Its full advance in user space, including its source adjustment.
    advance: f64,
    /// The advance a viewer applies when showing its codes (widths, Tc, Tw, Tz) in text space,
    /// so the rest of `advance` is written as a TJ number.
    shown: f64,
    space: bool,
    /// The first glyph of a source row after the paragraph's first, kept where it was.
    row_start: bool,
}

/// Assign inserted text the style at the insertion point; unchanged prefix/suffix characters
/// retain their source styles. A replace-all therefore has a predictable first-run style.
fn segments(block: &TextBlock, text: &str, range: Option<[usize; 2]>) -> Result<Vec<Segment>, EditError> {
    if block.runs.len() > MAX_SEGMENTS {
        return Err(EditError::Invalid("edit paragraphs with at most 8,192 style segments".into()));
    }
    let old: Vec<char> = block.text.chars().take(MAX_EDIT_CHARACTERS + 1).collect();
    let new: Vec<char> = text.chars().take(MAX_EDIT_CHARACTERS + 1).collect();
    if old.len() > MAX_EDIT_CHARACTERS || new.len() > MAX_EDIT_CHARACTERS {
        return Err(EditError::Invalid("edit paragraphs with at most 1,000,000 characters".into()));
    }
    if let Some([start, end]) = range
        && (start >= end || end > new.len())
    {
        return Err(EditError::Invalid(format!("selection must satisfy 0 <= start < end <= {} Unicode characters", new.len())));
    }
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old.iter().rev().zip(new.iter().rev()).take(old.len().min(new.len()).saturating_sub(prefix)).take_while(|(a, b)| a == b).count();
    let new_len = new.len();
    let insertion = prefix.min(old.len().saturating_sub(1));
    let mut out: Vec<Segment> = Vec::new();
    for (index, ch) in new.into_iter().enumerate() {
        let original = if index < prefix {
            index
        } else if index >= new_len.saturating_sub(suffix) {
            old.len().saturating_sub(new_len.saturating_sub(index))
        } else {
            insertion
        };
        let source = block.runs.partition_point(|r| r.end <= original).min(block.runs.len().saturating_sub(1));
        let source_start = block.runs.get(source).map_or(0, |r| r.start);
        let local = original.saturating_sub(source_start);
        let preserved = index < prefix || index >= new_len.saturating_sub(suffix);
        let selected = range.is_none_or(|[start, end]| index >= start && index < end);
        // PDF paragraph editing joins typed line separators; preserve deliberate repeated and
        // ideographic spaces instead of silently renumbering a user's selected characters.
        let ch = typed_char(ch);
        if let Some(last) = out.last_mut()
            && last.source == source
            && last.selected == selected
            && match &last.source_range {
                Some(r) => preserved && r.end == local,
                None => !preserved,
            }
        {
            last.text.push(ch);
            if let Some(r) = &mut last.source_range {
                r.end += 1;
            }
        } else {
            if out.len() >= MAX_SEGMENTS {
                return Err(EditError::Invalid("edit paragraphs with at most 8,192 style segments".into()));
            }
            out.push(Segment { text: ch.to_string(), source, selected, source_range: preserved.then_some(local..local + 1) });
        }
    }
    Ok(out)
}

#[derive(Eq, PartialEq, Hash)]
enum MetricsKey {
    Reference(pdfcraft_cos::ObjRef),
    // The paragraph's one page-local resource dictionary owns direct font dictionaries.
    Direct(String),
}

fn metrics(doc: &Document, fonts: &Dict, name: &str, cache: &mut HashMap<MetricsKey, Rc<Metrics>>) -> Option<Rc<Metrics>> {
    let font = fonts.get(name.as_bytes())?;
    let key = match font {
        Object::Ref(reference) => MetricsKey::Reference(*reference),
        _ => MetricsKey::Direct(name.to_owned()),
    };
    if let Some(metrics) = cache.get(&key) {
        return Some(metrics.clone());
    }
    let dictionary = doc.resolve(font).as_dict().cloned()?;
    let metrics = Rc::new(Metrics::from_dict(doc, &dictionary));
    cache.insert(key, metrics.clone());
    Some(metrics)
}

fn original_glyphs(source: &TextRun, range: &std::ops::Range<usize>, m: &Metrics) -> Result<Vec<SourceGlyph>, EditError> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    for glyph in &source.glyphs {
        let end = offset.saturating_add(glyph.text.chars().count());
        if offset < range.end && end > range.start {
            if offset < range.start || end > range.end {
                return Err(EditError::Invalid(
                    "the selection splits a PDF glyph mapped to several Unicode characters; select the whole glyph".into(),
                ));
            }
            out.push(glyph.clone());
        } else if offset == end && offset >= range.start && offset < range.end {
            out.push(glyph.clone());
        }
        offset = end;
    }
    // Paragraph joining introduces one logical separator between visual rows. It is not an
    // original PDF glyph and must be encoded in the adjacent run's own font.
    for _ in offset.max(range.start)..range.end {
        let bytes = m.encode(" ").ok_or_else(|| EditError::Invalid("the paragraph font cannot encode its line separator".into()))?;
        out.push(SourceGlyph { text: " ".into(), bytes, adjustment: 0.0 });
    }
    Ok(out)
}

fn prepare(
    doc: &mut Document,
    fonts: &mut Dict,
    block: &TextBlock,
    segments: &[Segment],
    style: &BlockStyle,
    k: f64,
    horizontal: f64,
) -> Result<Prepared, EditError> {
    let mut faces: Vec<Face> = Vec::new();
    let mut glyphs = Vec::new();
    let mut substituted = None;
    let mut metric_cache = HashMap::new();
    // Underlined glyphs cost a fixed number of operators however they are placed; refuse an
    // edit whose decorations alone exceed the budget before building the rest of it.
    let mut minimum = FRAME_OPS;
    for segment in segments {
        let source = block.runs.get(segment.source).ok_or_else(|| EditError::Invalid("the paragraph has no source style".into()))?;
        let mut state = source.state.clone();
        let change = segment.selected;
        let exact = change.then_some(style.font_resource.as_deref()).flatten();
        let mut name = exact.unwrap_or(&source.font).to_owned();
        let mut m = metrics(doc, fonts, &name, &mut metric_cache);
        let font_changed = change && (style.family.is_some() || style.family_only.is_some() || style.bold.is_some() || style.italic.is_some());
        let keep_codes = !font_changed && exact.is_none_or(|name| name == source.font) && segment.source_range.is_some();
        if exact.is_some() && m.as_ref().is_none_or(|m| m.encode(&segment.text).is_none()) {
            return Err(EditError::Invalid(format!("font resource /{name} is missing or cannot encode the selected characters")));
        }
        if font_changed || m.as_ref().is_none_or(|m| !keep_codes && m.encode(&segment.text).is_none()) {
            let (family, bold, italic) = if change {
                let (f, b, i) =
                    style.family.unwrap_or((style.family_only.unwrap_or_else(|| source_family(&source.base_font)), source.bold, source.italic));
                (f, style.bold.unwrap_or(b), style.italic.unwrap_or(i))
            } else {
                (source_family(&source.base_font), source.bold, source.italic)
            };
            if needs_type3(&segment.text) {
                let fallback = type3_font(doc, fonts, &segment.text, family, bold)?;
                name = fallback.name;
                state.word_spacing = 0.0;
                substituted = Some(format!("{} Type3", fallback.family));
            } else {
                name = standard_resource(doc, fonts, family.base_font(bold, italic))?;
                substituted = Some(family.base_font(bold, italic).into());
            }
            m = metrics(doc, fonts, &name, &mut metric_cache);
        }
        let m = m.ok_or_else(|| EditError::Invalid(format!("font /{name} has no usable metrics")))?;
        let size = if change { style.size.unwrap_or(source.size) / k } else { source.size / k };
        if !size.is_finite() || size <= 0.0 {
            return Err(EditError::Invalid("the source font size must be positive and finite for paragraph editing".into()));
        }
        state.font = Some((name, size));
        if change {
            if let Some(c) = style.color {
                state.fill = vec![Op::new("rg", c.into_iter().map(pdfcraft_content::num).collect())];
            }
            if let Some(c) = style.char_spacing {
                state.char_spacing = c / k;
            }
            if let Some(s) = style.scale {
                state.scale = s / 100.0;
            }
        }
        // Its source row starts here: an untouched row keeps that line break.
        let starts_row = keep_codes && source.row_start && segment.source_range.as_ref().is_some_and(|r| r.start == 0);
        let source_glyphs = if let Some(range) = &segment.source_range
            && keep_codes
        {
            original_glyphs(source, range, &m)?
        } else {
            let bytes = m.encode(&segment.text).ok_or_else(|| EditError::Invalid(format!("{} cannot encode the replacement text", m.base_font)))?;
            let mut offset = 0usize;
            let mut mapped = Vec::new();
            for (code, len) in m.codes(&bytes) {
                let end = offset.saturating_add(len);
                let data = bytes.get(offset..end).ok_or_else(|| EditError::Invalid("invalid font code length".into()))?;
                mapped.push(SourceGlyph { text: m.text_of(code).unwrap_or("").into(), bytes: data.to_vec(), adjustment: 0.0 });
                offset = end;
            }
            mapped
        };
        let underline = if change { style.underline.unwrap_or(source.underline) } else { source.underline };
        state.underline = underline;
        if underline && known_fill_color(&state.fill).is_none() {
            return Err(EditError::Invalid(
                "editable underlines currently need a device Gray, RGB or CMYK text color; choose an RGB color first".into(),
            ));
        }
        // Segments that end up with an identical face (a preserved prefix and the text typed
        // after it, say) share it, so their glyphs can be shown together.
        let face = match faces.last() {
            Some(last) if last.underline == underline && last.state == state => faces.len() - 1,
            _ => {
                faces.push(Face { state: state.clone(), metrics: m.clone(), underline });
                faces.len() - 1
            }
        };
        for (position, mut glyph) in source_glyphs.into_iter().enumerate() {
            let space = glyph.text == " ";
            let nominal: f64 = m
                .codes(&glyph.bytes)
                .iter()
                .map(|(code, len)| m.width(*code) * size + state.char_spacing + if m.is_space(*code, *len) { state.word_spacing } else { 0.0 })
                .sum();
            let old_size = source.state.font.as_ref().map_or(size, |(_, s)| *s).max(1e-6);
            let adjustment = glyph.adjustment * size / old_size * state.scale / source.state.scale.max(1e-6);
            let advance = (nominal * state.scale + adjustment) * horizontal;
            if !advance.is_finite() {
                return Err(EditError::Invalid("the font has invalid character advances".into()));
            }
            // A positioned logical space has no original bytes. Give its decoration a real
            // text association; the following explicit Tm still preserves its exact advance.
            if underline && space && glyph.bytes.is_empty() {
                glyph.bytes = m.encode(" ").ok_or_else(|| EditError::Invalid("the font cannot encode an underlined paragraph separator".into()))?;
            }
            // The advance a viewer gives these codes; a separator encoded just above is shown,
            // so it is measured from its bytes rather than taken from `nominal`.
            let shown = m
                .codes(&glyph.bytes)
                .iter()
                .map(|(code, len)| m.width(*code) * size + state.char_spacing + if m.is_space(*code, *len) { state.word_spacing } else { 0.0 })
                .sum::<f64>()
                * state.scale;
            if underline && !glyph.bytes.is_empty() {
                minimum = minimum.checked_add(UNDERLINED_GLYPH_OPS).filter(|n| *n <= MAX_EMITTED_OPS).ok_or_else(over_budget)?;
            }
            glyphs.push(Glyph { bytes: glyph.bytes, face, advance, shown, space, row_start: starts_row && position == 0 });
        }
    }
    Ok(Prepared { faces, glyphs, substituted })
}

/// Greedy rows of at most `width` (user space), breaking after spaces. With `keep_breaks`, a
/// source row start that follows a space also ends the row before it, so rows the edit does not
/// reach keep their words exactly where they were; an edit that overflows its row still wraps.
fn rows(glyphs: &[Glyph], width: f64, keep_breaks: bool) -> Vec<std::ops::Range<usize>> {
    // Re-adding the advances of a row the source fitted must not wrap it on rounding.
    let width = width + WIDTH_TOLERANCE;
    let mut rows = Vec::new();
    let (mut start, mut advance, mut word_break) = (0, 0.0, None);
    let mut index = 0;
    while let Some(glyph) = glyphs.get(index) {
        if keep_breaks && glyph.row_start && index > start && index.checked_sub(1).and_then(|i| glyphs.get(i)).is_some_and(|g| g.space) {
            rows.push(start..index);
            start = index;
            advance = 0.0;
            word_break = None;
        }
        if glyph.space {
            word_break = Some(index + 1);
        }
        if advance + glyph.advance > width
            && index > start
            && let Some(end) = word_break.filter(|end| *end > start && *end <= index)
        {
            rows.push(start..end);
            start = end;
            index = end;
            advance = 0.0;
            word_break = None;
            continue;
        }
        advance += glyph.advance;
        index += 1;
    }
    if start < glyphs.len() {
        rows.push(start..glyphs.len());
    }
    rows
}

/// One placement in the planned paragraph.
enum Item {
    /// Consecutive glyphs of one face on one row: each glyph with the TJ adjustment (text
    /// space) that follows it.
    Chunk { face: usize, x: f64, row: usize, glyphs: Vec<(usize, f64)> },
    /// An underlined glyph, placed, marked and decorated on its own.
    Underlined { face: usize, x: f64, row: usize, glyph: usize, width: f64 },
}

impl Item {
    fn face(&self) -> usize {
        match self {
            Item::Chunk { face, .. } | Item::Underlined { face, .. } => *face,
        }
    }
}

/// Place the glyphs of `rows`. A row that wraps at a single space drops that space when it has
/// the style of the character before it: the reader joins rows with one space in that
/// character's style, so text and styles read back unchanged. Repeated spaces at a wrap are
/// all kept (the reader adds no space after them). Alignment and justification measure each
/// row without its trailing spaces.
fn plan(glyphs: &[Glyph], faces: &[Face], rows: &[std::ops::Range<usize>], width: f64, horizontal: f64, style: &BlockStyle) -> Vec<Item> {
    let same_face = |a: &Glyph, b: &Glyph| match (faces.get(a.face), faces.get(b.face)) {
        (Some(a), Some(b)) => a.underline == b.underline && a.state == b.state,
        _ => false,
    };
    let mut items = Vec::new();
    for (row_index, range) in rows.iter().enumerate() {
        let last_row = row_index + 1 == rows.len();
        let mut end = range.end;
        let wrap_space = end.checked_sub(1).filter(|i| *i > range.start).and_then(|i| Some((glyphs.get(i)?, glyphs.get(i - 1)?)));
        if !last_row && wrap_space.is_some_and(|(space, before)| space.space && !before.space && same_face(space, before)) {
            end -= 1;
        }
        let line = glyphs.get(range.start..end).unwrap_or_default();
        let trailing = line.iter().rev().take_while(|g| g.space).count();
        let measured = line.get(..line.len().saturating_sub(trailing)).unwrap_or_default();
        let advance: f64 = measured.iter().map(|g| g.advance).sum();
        let free = (width - advance).max(0.0);
        let mut x = match style.align {
            Some(crate::added::Align::Center) => free / 2.0,
            Some(crate::added::Align::Right) => free,
            _ => 0.0,
        } / horizontal;
        let spaces = measured.iter().filter(|g| g.space).count();
        let justify = style.align == Some(crate::added::Align::Justify) && !last_row && spaces > 0;
        let extra = if justify { free / horizontal / spaces as f64 } else { 0.0 };
        let mut chunk: Option<Item> = None;
        for (offset, glyph) in line.iter().enumerate() {
            let index = range.start + offset;
            let width = glyph.advance / horizontal + if glyph.space && offset < measured.len() { extra } else { 0.0 };
            let underlined = faces.get(glyph.face).is_some_and(|f| f.underline) && !glyph.bytes.is_empty();
            if underlined || glyph.bytes.is_empty() {
                // A code-less gap or a decorated glyph ends the current placement.
                items.extend(chunk.take());
                if underlined {
                    items.push(Item::Underlined { face: glyph.face, x, row: row_index, glyph: index, width });
                }
            } else {
                match &mut chunk {
                    Some(Item::Chunk { face, glyphs, .. }) if *face == glyph.face => glyphs.push((index, width - glyph.shown)),
                    _ => {
                        items.extend(chunk.take());
                        chunk = Some(Item::Chunk { face: glyph.face, x, row: row_index, glyphs: vec![(index, width - glyph.shown)] });
                    }
                }
            }
            x += width;
        }
        items.extend(chunk);
    }
    items
}

/// Exactly how many operators [`rewrite`] emits for `items`.
fn count(items: &[Item], faces: &[Face]) -> usize {
    let mut total = FRAME_OPS;
    let mut current = None;
    for item in items {
        if current != Some(item.face()) {
            current = Some(item.face());
            total = total.saturating_add(faces.get(item.face()).map_or(0, |f| f.state.op_count()));
        }
        total = total.saturating_add(match item {
            Item::Chunk { .. } => CHUNK_OPS,
            Item::Underlined { .. } => UNDERLINED_GLYPH_OPS,
        });
    }
    total
}

/// The `Tj`/`TJ` showing a chunk: its glyphs' codes, with each adjustment (text space) after its
/// glyph as a TJ number in thousandths of the face's scaled size.
fn show(glyphs: &[Glyph], chunk: &[(usize, f64)], face: &Face) -> Result<Op, EditError> {
    let size = face.state.font.as_ref().map_or(0.0, |(_, s)| *s);
    let unit = size * face.state.scale / 1000.0;
    if !(unit.is_finite() && unit > 0.0) {
        return Err(EditError::Invalid("the paragraph font size must be positive and finite".into()));
    }
    let mut parts: Vec<Object> = Vec::new();
    let mut string = Vec::new();
    for (position, (index, adjustment)) in chunk.iter().enumerate() {
        let glyph = glyphs.get(*index).ok_or_else(|| EditError::Invalid("the paragraph lost a glyph".into()))?;
        string.extend_from_slice(&glyph.bytes);
        // Sub-micro-point residue from rounding is not an adjustment; nothing follows the last.
        if adjustment.abs() > 1e-7 && position + 1 < chunk.len() {
            let number = -adjustment / unit;
            if !number.is_finite() {
                return Err(EditError::Invalid("the font has invalid character advances".into()));
            }
            parts.push(Object::String(PdfString::literal(std::mem::take(&mut string))));
            parts.push(pdfcraft_content::num(number));
        }
    }
    if parts.is_empty() {
        return Ok(Op::new("Tj", vec![Object::String(PdfString::literal(string))]));
    }
    if !string.is_empty() {
        parts.push(Object::String(PdfString::literal(string)));
    }
    Ok(Op::new("TJ", vec![Object::Array(parts)]))
}

/// Rewrite `block` with `text` through the run-preserving path. `fake_bold` also removes exact,
/// near-coincident copies of the paragraph's lines (see [`coincident_ops`]); callers pass it
/// only for whole-paragraph edits of a uniform, undecorated paragraph.
pub(super) fn rewrite(
    doc: &mut Document,
    page: usize,
    lines: &[TextLine],
    block: &TextBlock,
    text: &str,
    style: &BlockStyle,
    fake_bold: bool,
) -> Result<LineEdit, EditError> {
    let first = block.lines.first().and_then(|i| lines.get(*i)).ok_or_else(|| EditError::Invalid("the paragraph has no source line".into()))?;
    let o = &first.origin;
    let k = o.k.max(1e-6);
    if !supported_frame(lines, block) {
        return Err(EditError::Invalid(
            "mixed-run editing needs upright or rotated text without skew or reflection; skewed or reflected text is preserved unchanged".into(),
        ));
    }
    // Advances along the writing direction, in user space per text-space unit.
    let horizontal = horizontal_scale(o);
    let frame = frame_of(o);
    let frame_rect = frame_box(lines, block);
    let p = page_dict(doc, page)?;
    let res = p.dict.get(b"Resources").and_then(|r| doc.resolve(r).as_dict().cloned()).unwrap_or_default();
    let original_fonts = res.get(b"Font").and_then(|f| doc.resolve(f).as_dict().cloned()).unwrap_or_default();
    let mut fonts = original_fonts.clone();
    let segments = segments(block, text, style.range)?;
    let Prepared { faces, glyphs, substituted } = prepare(doc, &mut fonts, block, &segments, style, k, horizontal)?;
    let mut next_decoration = if faces.iter().any(|face| face.underline) {
        let pieces = Pieces::join(&content_streams(doc, &p.dict).into_iter().map(|(_, data)| data).collect::<Vec<_>>());
        decorations::Index::read(&pieces.parse().ops).next_id()?
    } else {
        1
    };
    let [dx, dy] = style.offset.unwrap_or([0.0, 0.0]);
    // Widths are measured along the writing direction; a single line grows to the page margin
    // it runs towards (the right margin for upright text).
    let width = style.width.map(|w| w.clamp(block.size, 14_400.0)).unwrap_or_else(|| {
        let current = (frame_rect[2] - frame_rect[0]).max(block.size);
        if block.line_count == 1 {
            let (x, y) = frame.user((frame_rect[0], o.baseline));
            current.max(room_to_margin(p.crop(doc), &frame, (x + dx, y + dy)))
        } else {
            current
        }
    });
    // Source line breaks are kept unless the edit changes how wide the text is set.
    let keep_breaks = style.width.is_none()
        && style.size.is_none()
        && style.font_resource.is_none()
        && style.family.is_none()
        && style.family_only.is_none()
        && style.bold.is_none()
        && style.italic.is_none()
        && style.char_spacing.is_none()
        && style.scale.is_none();
    let rows = rows(&glyphs, width, keep_breaks);
    // Line spacing in text space: requested, else the paragraph's own (or its TL for a single
    // line) scaled with the largest size when sizes change, else 1.2 × the largest size.
    let old_lead = block.lines.iter().filter_map(|i| lines.get(*i)).map(|l| o.baseline - l.origin.baseline).find(|d| *d > block.size * 0.5);
    let max_size = faces.iter().filter_map(|f| f.state.font.as_ref().map(|(_, s)| *s)).fold(0.0, f64::max);
    let max_size = if max_size > 0.0 { max_size } else { block.size / k };
    let old_max = block.runs.iter().map(|r| r.size / k).fold(0.0, f64::max);
    let ratio = if old_max > 0.0 && (max_size / old_max).is_finite() { max_size / old_max } else { 1.0 };
    let lead = style
        .line_spacing
        .map(|n| n * max_size)
        .or_else(|| old_lead.map(|v| v / k * ratio))
        .or_else(|| (o.state.leading > 0.0).then_some(o.state.leading * ratio))
        .unwrap_or(max_size * 1.2);
    if !lead.is_finite() {
        return Err(EditError::Invalid("the paragraph has an invalid line spacing".into()));
    }
    let items = plan(&glyphs, &faces, &rows, width, horizontal, style);
    let emitted = count(&items, &faces);
    if emitted > MAX_EMITTED_OPS {
        return Err(over_budget());
    }
    let n = pdfcraft_content::num;
    let mut ops = Vec::with_capacity(emitted);
    ops.push(Op::new("q", vec![]));
    if dx != 0.0 || dy != 0.0 {
        let c = o.ctm;
        let inverse =
            Matrix([c[0], c[1], c[2], c[3], 0.0, 0.0]).invert().ok_or_else(|| EditError::Invalid("the paragraph has a singular transform".into()))?;
        let (x, y) = inverse.apply(dx, dy);
        ops.push(Op::new("cm", vec![n(1.0), n(0.0), n(0.0), n(1.0), n(x), n(y)]));
    }
    ops.push(Op::new("BT", vec![]));
    let mut underlines = Vec::new();
    let mut current = None;
    for item in &items {
        let face = faces.get(item.face()).ok_or_else(|| EditError::Invalid("the run's font was lost".into()))?;
        if current != Some(item.face()) {
            current = Some(item.face());
            ops.extend(face.state.ops());
        }
        let (x, row) = match item {
            Item::Chunk { x, row, .. } | Item::Underlined { x, row, .. } => (*x, *row),
        };
        let tm = Matrix([1.0, 0.0, 0.0, 1.0, x, -(row as f64) * lead]).then(&Matrix(o.tm));
        ops.push(Op::new("Tm", matrix_operands(tm.0)));
        match item {
            Item::Chunk { glyphs: chunk, .. } => ops.push(show(&glyphs, chunk, face)?),
            Item::Underlined { glyph, width, .. } => {
                let glyph = glyphs.get(*glyph).ok_or_else(|| EditError::Invalid("the paragraph lost a glyph".into()))?;
                let id = next_decoration;
                next_decoration =
                    next_decoration.checked_add(1).ok_or_else(|| EditError::Invalid("the page has exhausted underline association IDs".into()))?;
                ops.push(decorations::begin(id, false));
                ops.push(Op::new("Tj", vec![Object::String(PdfString::literal(glyph.bytes.clone()))]));
                ops.push(Op::new("EMC", vec![]));
                let size = face.state.font.as_ref().map_or(max_size, |(_, s)| *s);
                let y = -(row as f64) * lead + face.state.rise + face.metrics.descent * size * 0.5;
                if ![x, x + width, y].into_iter().all(f64::is_finite) {
                    return Err(EditError::Invalid("the font has invalid underline geometry".into()));
                }
                underlines.push((id, x, x + width, y, size, fill_color(&face.state.fill)));
            }
        }
    }
    ops.push(Op::new("ET", vec![]));
    for (id, x0, x1, y, size, color) in underlines {
        ops.push(decorations::begin(id, true));
        ops.push(Op::new("q", vec![]));
        ops.push(Op::new("cm", matrix_operands(o.tm)));
        ops.push(Op::new("RG", color.into_iter().map(n).collect()));
        ops.push(Op::new("w", vec![n(size * 0.06)]));
        ops.push(Op::new("J", vec![Object::Int(0)]));
        ops.push(Op::new("d", vec![Object::Array(Vec::new()), Object::Int(0)]));
        ops.push(Op::new("m", vec![n(x0), n(y)]));
        ops.push(Op::new("l", vec![n(x1), n(y)]));
        ops.push(Op::new("S", vec![]));
        ops.push(Op::new("Q", vec![]));
        ops.push(Op::new("EMC", vec![]));
    }
    ops.push(Op::new("Q", vec![]));
    // The count is exact by construction; keep the bound enforced even if emission and plan
    // ever drift apart.
    if ops.len() > emitted {
        return Err(EditError::Invalid(format!(
            "internal error: the paragraph emitted {} operators but budgeted {emitted}; the document was not changed",
            ops.len()
        )));
    }
    // Only a paragraph that gained a font resource rewrites the page resources; otherwise a
    // shared or indirect resource dictionary stays exactly as it was.
    let fonts = (fonts != original_fonts).then_some(fonts);
    install_block(doc, page, lines, block, ops, fonts, !fake_bold)?;
    Ok(LineEdit { substituted })
}
