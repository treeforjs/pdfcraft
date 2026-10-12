//! Run-preserving paragraph reflow. Character offsets refer to Unicode scalars; PDF character
//! codes are recovered through each run's own encoding, including multi-scalar ToUnicode maps.

use super::*;

pub(super) const MAX_SEGMENTS: usize = 8192;
const MAX_EDIT_CHARACTERS: usize = 1_000_000;
/// Run-preserving reflow places every glyph explicitly: each one replays its text state, and an
/// underlined glyph adds two association markers and its own path. The character ceiling alone
/// would allow tens of millions of in-memory operators, so the emitted count is budgeted while
/// the glyphs are prepared, before any of them is emitted. About 150 bytes per operator keeps the
/// worst case to a few hundred megabytes; ordinary paragraphs use a tiny fraction of it.
pub(super) const MAX_EMITTED_OPS: usize = 2_000_000;
/// Operators emitted per glyph besides its state: `Tm` and `Tj`.
const GLYPH_OPS: usize = 2;
/// An underlined glyph's text marker pair plus its marked path
/// (`BDC q cm RG w J d m l S Q EMC`).
const UNDERLINE_OPS: usize = 2 + 12;
/// Paragraph framing: `q`, an optional `cm`, `BT`, `ET` and the closing `Q`.
const FRAME_OPS: usize = 5;

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
    /// Upper bound on the operators [`rewrite`] emits, already checked against
    /// [`MAX_EMITTED_OPS`].
    emitted: usize,
}

struct Glyph {
    bytes: Vec<u8>,
    face: usize,
    advance: f64,
    space: bool,
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
        let ch = if matches!(ch, '\n' | '\r' | '\t') { ' ' } else { ch };
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
    let mut faces = Vec::new();
    let mut glyphs = Vec::new();
    let mut substituted = None;
    let mut metric_cache = HashMap::new();
    let mut emitted = FRAME_OPS;
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
        let face = faces.len();
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
        let state_ops = state.op_count();
        for mut glyph in source_glyphs {
            // Budget this glyph before keeping it. Whether it is decorated is decided below by
            // the same rule `rewrite` uses; counting the larger case keeps this an upper bound.
            let per_glyph = state_ops.saturating_add(GLYPH_OPS).saturating_add(if underline { UNDERLINE_OPS } else { 0 });
            emitted = emitted.checked_add(per_glyph).filter(|n| *n <= MAX_EMITTED_OPS).ok_or_else(over_budget)?;
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
            glyphs.push(Glyph { bytes: glyph.bytes, face, advance, space });
        }
        faces.push(Face { state, metrics: m, underline });
    }
    Ok(Prepared { faces, glyphs, substituted, emitted })
}

fn rows(glyphs: &[Glyph], width: f64) -> Vec<std::ops::Range<usize>> {
    let mut rows = Vec::new();
    let (mut start, mut advance, mut word_break) = (0, 0.0, None);
    let mut index = 0;
    while let Some(glyph) = glyphs.get(index) {
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

pub(super) fn rewrite(
    doc: &mut Document,
    page: usize,
    lines: &[TextLine],
    block: &TextBlock,
    text: &str,
    style: &BlockStyle,
) -> Result<LineEdit, EditError> {
    let first = block.lines.first().and_then(|i| lines.get(*i)).ok_or_else(|| EditError::Invalid("the paragraph has no source line".into()))?;
    let o = &first.origin;
    let k = o.k.max(1e-6);
    let transform = Matrix(o.tm).then(&Matrix(o.ctm)).0;
    if transform[1].abs() > 1e-8
        || transform[2].abs() > 1e-8
        || transform[0] <= 0.0
        || transform[3] <= 0.0
        || block.runs.iter().any(|r| !r.state.scale.is_finite() || r.state.scale <= 0.0)
    {
        return Err(EditError::Invalid(
            "mixed-run editing currently needs upright text; rotated, reflected or skewed text is preserved unchanged".into(),
        ));
    }
    let horizontal = transform[0];
    let p = page_dict(doc, page)?;
    let res = p.dict.get(b"Resources").and_then(|r| doc.resolve(r).as_dict().cloned()).unwrap_or_default();
    let mut fonts = res.get(b"Font").and_then(|f| doc.resolve(f).as_dict().cloned()).unwrap_or_default();
    let segments = segments(block, text, style.range)?;
    let Prepared { faces, glyphs, substituted, emitted } = prepare(doc, &mut fonts, block, &segments, style, k, horizontal)?;
    let mut next_decoration = if faces.iter().any(|face| face.underline) {
        let pieces = Pieces::join(&content_streams(doc, &p.dict).into_iter().map(|(_, data)| data).collect::<Vec<_>>());
        decorations::Index::read(&pieces.parse().ops).next_id()?
    } else {
        1
    };
    let [dx, dy] = style.offset.unwrap_or([0.0, 0.0]);
    let width = style.width.map(|w| w.clamp(block.size, 14_400.0)).unwrap_or_else(|| {
        let current = (block.rect[2] - block.rect[0]).max(block.size);
        if block.line_count == 1 { current.max(p.crop(doc)[2] - 36.0 - block.rect[0] - dx) } else { current }
    });
    let rows = rows(&glyphs, width);
    let old_lead = block.lines.iter().filter_map(|i| lines.get(*i)).map(|l| o.baseline - l.origin.baseline).find(|d| *d > block.size * 0.5);
    let max_size = faces.iter().filter_map(|f| f.state.font.as_ref().map(|(_, s)| *s)).fold(block.size / k, f64::max);
    let lead = style.line_spacing.map(|n| n * max_size).or_else(|| old_lead.map(|v| v / k)).unwrap_or(max_size * 1.2);
    let n = pdfcraft_content::num;
    // `emitted` was bounded by MAX_EMITTED_OPS during preparation.
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
    for (row_index, range) in rows.iter().enumerate() {
        let line = glyphs.get(range.clone()).unwrap_or_default();
        let advance: f64 = line.iter().map(|g| g.advance).sum();
        let free = (width - advance).max(0.0);
        let mut x = match style.align {
            Some(crate::added::Align::Center) => free / 2.0,
            Some(crate::added::Align::Right) => free,
            _ => 0.0,
        } / horizontal;
        let spaces = line.iter().filter(|g| g.space).count();
        let justify = style.align == Some(crate::added::Align::Justify) && row_index + 1 < rows.len() && spaces > 0;
        let extra = if justify { free / horizontal / spaces as f64 } else { 0.0 };
        // Every glyph's placement is explicit. This makes justification work with a Type 3
        // font whose space code is not 32 and avoids leaking one run's Tz/Tw into another.
        for glyph in line {
            let face = faces.get(glyph.face).ok_or_else(|| EditError::Invalid("the run's font was lost".into()))?;
            ops.extend(face.state.ops());
            let tm = Matrix([1.0, 0.0, 0.0, 1.0, x, -(row_index as f64) * lead]).then(&Matrix(o.tm));
            ops.push(Op::new("Tm", tm.0.into_iter().map(n).collect()));
            let decoration = (face.underline && !glyph.bytes.is_empty()).then_some(next_decoration);
            if let Some(id) = decoration {
                next_decoration =
                    next_decoration.checked_add(1).ok_or_else(|| EditError::Invalid("the page has exhausted underline association IDs".into()))?;
                ops.push(decorations::begin(id, false));
            }
            ops.push(Op::new("Tj", vec![Object::String(PdfString::literal(glyph.bytes.clone()))]));
            if decoration.is_some() {
                ops.push(Op::new("EMC", vec![]));
            }
            let width = glyph.advance / horizontal + if glyph.space { extra } else { 0.0 };
            if let Some(id) = decoration {
                let size = face.state.font.as_ref().map_or(max_size, |(_, s)| *s);
                let y = -(row_index as f64) * lead + face.state.rise + face.metrics.descent * size * 0.5;
                if ![x, x + width, y].into_iter().all(f64::is_finite) {
                    return Err(EditError::Invalid("the font has invalid underline geometry".into()));
                }
                underlines.push((id, x, x + width, y, size, fill_color(&face.state.fill)));
            }
            x += width;
        }
    }
    ops.push(Op::new("ET", vec![]));
    if !underlines.is_empty() {
        for (id, x0, x1, y, size, color) in underlines {
            ops.push(decorations::begin(id, true));
            ops.push(Op::new("q", vec![]));
            ops.push(Op::new("cm", o.tm.into_iter().map(n).collect()));
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
    }
    ops.push(Op::new("Q", vec![]));
    // The preparation estimate is an upper bound by construction; keep the bound enforced
    // even if emission and estimate ever drift apart.
    if ops.len() > emitted {
        return Err(EditError::Invalid(format!(
            "internal error: the paragraph emitted {} operators but budgeted {emitted}; the document was not changed",
            ops.len()
        )));
    }
    install_block(doc, page, lines, block, ops, Some(fonts), true)?;
    Ok(LineEdit { substituted })
}
