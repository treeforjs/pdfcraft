//! Edit a PDF ▸ Edit text: the text lines already on a page, and replacing a line's text in place.
//!
//! A *line* is a run of text-showing operators inside one `BT … ET` on the same baseline, read
//! from one content stream. Replacing it rewrites only that stream (as a new object, since content
//! streams may be shared): the first operator of the line shows the new text, the others are
//! dropped, and every positioning operator stays, so the lines after it keep their places
//! (`Td`, `T*` and friends move from the line matrix, which showing text doesn't change).
//!
//! The new text uses the line's own font when every character has a code and a glyph in it;
//! otherwise it is set in Helvetica (WinAnsi), and the result says the font was substituted.
//! Reflowing a paragraph to a new width is not done here (see `edit.text-reflow`).

use std::collections::HashMap;
use std::rc::Rc;

use printcraft_content::{Matrix, Op, parse, serialize_ops};
use printcraft_cos::{Dict, Document, Object, PdfString, Stream};
use printcraft_fonts::pdf::Metrics;

use crate::EditError;

/// One line of existing text.
#[derive(Clone, Debug, PartialEq)]
pub struct TextLine {
    /// The line's text as shown.
    pub text: String,
    /// Its box in user space.
    pub rect: [f64; 4],
    /// The font's resource name and `/BaseFont`, and its size in text space.
    pub font: String,
    pub base_font: String,
    pub size: f64,
    /// Whether new text in this font can only be shown by substituting another font (no
    /// Unicode mapping for the line, so nothing could be reused).
    pub decodable: bool,
    stream: usize,
    ops: Vec<usize>,
}

/// What replacing a line did.
#[derive(Clone, Debug, PartialEq)]
pub struct LineEdit {
    /// `Some(font)` when the line's font couldn't show the new text and Helvetica was used.
    pub substituted: Option<String>,
}

#[derive(Clone)]
struct Ts {
    ctm: Matrix,
    font: Option<(Vec<u8>, Rc<Metrics>)>,
    size: f64,
    char_spacing: f64,
    word_spacing: f64,
    scale: f64,
    leading: f64,
    rise: f64,
}

fn content_streams(doc: &Document, page: &Dict) -> Vec<(Object, Vec<u8>)> {
    let list: Vec<Object> = match page.get(b"Contents") {
        None => Vec::new(),
        Some(c) => match &*doc.resolve(c) {
            Object::Array(a) => a.clone(),
            _ => vec![c.clone()],
        },
    };
    list.into_iter()
        .map(|o| {
            let data = match &*doc.resolve(&o) {
                Object::Stream(s) => s.decoded().unwrap_or_default(),
                _ => Vec::new(),
            };
            (o, data)
        })
        .collect()
}

fn page_dict(doc: &Document, page: usize) -> Result<printcraft_model::Page, EditError> {
    printcraft_model::pages(doc).into_iter().nth(page).ok_or(EditError::NoSuchPage(page))
}

/// The text-showing operators of one stream with their text, font, box and baseline.
struct Shown {
    op: usize,
    text: String,
    rect: [f64; 4],
    baseline: f64,
    start_x: f64,
    end_x: f64,
    bt: usize,
    font: Vec<u8>,
    base_font: String,
    size: f64,
    decodable: bool,
}

fn interpret(doc: &Document, ops: &[Op], fonts_res: &Dict, cache: &mut HashMap<Vec<u8>, Rc<Metrics>>) -> Vec<Shown> {
    let mut out = Vec::new();
    let mut ts = Ts { ctm: Matrix::IDENTITY, font: None, size: 0.0, char_spacing: 0.0, word_spacing: 0.0, scale: 1.0, leading: 0.0, rise: 0.0 };
    let mut stack: Vec<Ts> = Vec::new();
    let (mut tm, mut tlm) = (Matrix::IDENTITY, Matrix::IDENTITY);
    let mut bt = 0usize;
    for (i, op) in ops.iter().enumerate() {
        match op.op.as_slice() {
            b"q" => stack.push(ts.clone()),
            b"Q" => {
                if let Some(s) = stack.pop() {
                    ts = s;
                }
            }
            b"cm" => {
                if let Some(m) = op.nums::<6>() {
                    ts.ctm = Matrix(m).then(&ts.ctm);
                }
            }
            b"BT" => {
                tm = Matrix::IDENTITY;
                tlm = Matrix::IDENTITY;
                bt += 1;
            }
            b"Tf" => {
                ts.size = op.num(1).unwrap_or(ts.size);
                if let Some(name) = op.name(0) {
                    let m = cache.entry(name.to_vec()).or_insert_with(|| {
                        Rc::new(
                            fonts_res
                                .get(name)
                                .and_then(|f| doc.resolve(f).as_dict().cloned())
                                .map(|d| Metrics::from_dict(doc, &d))
                                .unwrap_or_else(Metrics::fallback),
                        )
                    });
                    ts.font = Some((name.to_vec(), m.clone()));
                }
            }
            b"Tc" => ts.char_spacing = op.num(0).unwrap_or(0.0),
            b"Tw" => ts.word_spacing = op.num(0).unwrap_or(0.0),
            b"Tz" => ts.scale = op.num(0).unwrap_or(100.0) / 100.0,
            b"TL" => ts.leading = op.num(0).unwrap_or(0.0),
            b"Ts" => ts.rise = op.num(0).unwrap_or(0.0),
            b"Td" | b"TD" => {
                if let Some([x, y]) = op.nums::<2>() {
                    if op.is("TD") {
                        ts.leading = -y;
                    }
                    tlm = Matrix([1.0, 0.0, 0.0, 1.0, x, y]).then(&tlm);
                    tm = tlm;
                }
            }
            b"Tm" => {
                if let Some(m) = op.nums::<6>() {
                    tlm = Matrix(m);
                    tm = tlm;
                }
            }
            b"T*" => {
                tlm = Matrix([1.0, 0.0, 0.0, 1.0, 0.0, -ts.leading]).then(&tlm);
                tm = tlm;
            }
            b"Tj" | b"TJ" | b"'" | b"\"" => {
                if matches!(op.op.as_slice(), b"'" | b"\"") {
                    if op.is("\"") {
                        ts.word_spacing = op.num(0).unwrap_or(ts.word_spacing);
                        ts.char_spacing = op.num(1).unwrap_or(ts.char_spacing);
                    }
                    tlm = Matrix([1.0, 0.0, 0.0, 1.0, 0.0, -ts.leading]).then(&tlm);
                    tm = tlm;
                }
                let Some((name, m)) = ts.font.clone() else { continue };
                // Pieces: strings, and TJ number adjustments.
                let pieces: Vec<Object> = match op.op.as_slice() {
                    b"TJ" => op.operands.first().and_then(Object::as_array).cloned().unwrap_or_default(),
                    _ => op.operands.last().cloned().into_iter().collect(),
                };
                let trm0 = tm.then(&ts.ctm);
                let start = trm0.apply(0.0, ts.rise);
                let mut text = String::new();
                let mut decodable = true;
                let mut x_text = 0.0;
                for p in &pieces {
                    match p {
                        Object::String(s) => {
                            for (code, len) in m.codes(&s.bytes) {
                                match m.text_of(code) {
                                    Some(t) => text.push_str(t),
                                    None => decodable = false,
                                }
                                let w = m.width(code) * ts.size + ts.char_spacing + if m.is_space(code, len) { ts.word_spacing } else { 0.0 };
                                x_text += w * ts.scale;
                            }
                        }
                        other => {
                            if let Some(n) = other.as_f64() {
                                let dx = -n / 1000.0 * ts.size * ts.scale;
                                // A large gap inside TJ reads as a space.
                                if dx > ts.size * 0.2 && !text.ends_with(' ') {
                                    text.push(' ');
                                }
                                x_text += dx;
                            }
                        }
                    }
                }
                tm = Matrix([1.0, 0.0, 0.0, 1.0, x_text, 0.0]).then(&tm);
                let end = tm.then(&ts.ctm).apply(0.0, ts.rise);
                // The box: from descent to ascent along the run.
                let corners = [
                    trm0.apply(0.0, ts.rise + m.descent * ts.size),
                    trm0.apply(x_text, ts.rise + m.descent * ts.size),
                    trm0.apply(0.0, ts.rise + m.ascent * ts.size),
                    trm0.apply(x_text, ts.rise + m.ascent * ts.size),
                ];
                let rect = corners
                    .iter()
                    .fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |b, p| [b[0].min(p.0), b[1].min(p.1), b[2].max(p.0), b[3].max(p.1)]);
                let size_user = (trm0.0[2].powi(2) + trm0.0[3].powi(2)).sqrt() * ts.size;
                out.push(Shown {
                    op: i,
                    text,
                    rect,
                    baseline: start.1,
                    start_x: start.0,
                    end_x: end.0,
                    bt,
                    font: name,
                    base_font: m.base_font.clone(),
                    size: if size_user > 0.0 { size_user } else { ts.size },
                    decodable,
                });
            }
            _ => {}
        }
    }
    out
}

/// The lines of text on a page (0-based), in content order.
pub fn text_lines(doc: &Document, page: usize) -> Result<Vec<TextLine>, EditError> {
    let p = page_dict(doc, page)?;
    let res = p.dict.get(b"Resources").map(|r| doc.resolve(r)).and_then(|r| r.as_dict().cloned()).unwrap_or_default();
    let fonts_res = res.get(b"Font").map(|f| doc.resolve(f)).and_then(|f| f.as_dict().cloned()).unwrap_or_default();
    let mut cache = HashMap::new();
    let mut lines: Vec<TextLine> = Vec::new();
    for (si, (_, data)) in content_streams(doc, &p.dict).into_iter().enumerate() {
        let ops = parse(&data).ops;
        let shown = interpret(doc, &ops, &fonts_res, &mut cache);
        let mut last: Option<(usize, f64, f64, f64)> = None; // (bt, baseline, end_x, size)
        for s in shown {
            let joins = last.is_some_and(|(bt, base, end, size)| {
                bt == s.bt && (s.baseline - base).abs() < size * 0.3 && s.start_x > end - size && s.start_x - end < size * 3.0
            });
            if joins && let Some(l) = lines.last_mut() {
                let gap = s.start_x - last.map_or(s.start_x, |x| x.2);
                if gap > l.size * 0.2 && !l.text.ends_with(' ') && !s.text.starts_with(' ') {
                    l.text.push(' ');
                }
                l.text.push_str(&s.text);
                l.rect = [l.rect[0].min(s.rect[0]), l.rect[1].min(s.rect[1]), l.rect[2].max(s.rect[2]), l.rect[3].max(s.rect[3])];
                l.ops.push(s.op);
                l.decodable &= s.decodable;
            } else {
                lines.push(TextLine {
                    text: s.text.clone(),
                    rect: s.rect,
                    font: String::from_utf8_lossy(&s.font).into_owned(),
                    base_font: s.base_font.clone(),
                    size: s.size,
                    decodable: s.decodable,
                    stream: si,
                    ops: vec![s.op],
                });
            }
            last = Some((s.bt, s.baseline, s.end_x, s.size.max(1.0)));
        }
    }
    // Lines of only spaces aren't editable text.
    lines.retain(|l| !l.text.trim().is_empty());
    Ok(lines)
}

/// The substitute font's resource name.
const SUBSTITUTE: &[u8] = b"PCEdHelv";

/// Replace the text of line `line` (an index into [`text_lines`]) on `page` with `text`.
pub fn replace_line(doc: &mut Document, page: usize, line: usize, text: &str) -> Result<LineEdit, EditError> {
    let text = text.replace(['\n', '\r'], " ");
    let lines = text_lines(doc, page)?;
    let target = lines.get(line).cloned().ok_or_else(|| EditError::Invalid(format!("page {} has no line {}", page + 1, line + 1)))?;
    let p = page_dict(doc, page)?;
    let mut res = p.dict.get(b"Resources").map(|r| doc.resolve(r)).and_then(|r| r.as_dict().cloned()).unwrap_or_default();
    let mut fonts_res = res.get(b"Font").map(|f| doc.resolve(f)).and_then(|f| f.as_dict().cloned()).unwrap_or_default();
    let streams = content_streams(doc, &p.dict);
    let (stream_obj, data) = streams.get(target.stream).cloned().ok_or_else(|| EditError::Invalid("the page's content changed".into()))?;
    let mut ops = parse(&data).ops;
    let first = target.ops[0];
    // The line's own font, when it can show every character.
    let font = fonts_res.get(target.font.as_bytes()).and_then(|f| doc.resolve(f).as_dict().cloned()).map(|d| Metrics::from_dict(doc, &d));
    let reused = font.as_ref().and_then(|m| m.encode(&text));
    let mut substituted = None;
    let mut replacement: Vec<Op> = Vec::new();
    // ' and " also move to the next line; keep that.
    match ops[first].op.as_slice() {
        b"'" => replacement.push(Op::new("T*", vec![])),
        b"\"" => {
            replacement.push(Op::new("Tw", vec![ops[first].operands.first().cloned().unwrap_or(Object::Int(0))]));
            replacement.push(Op::new("Tc", vec![ops[first].operands.get(1).cloned().unwrap_or(Object::Int(0))]));
            replacement.push(Op::new("T*", vec![]));
        }
        _ => {}
    }
    match reused {
        Some(bytes) => replacement.push(Op::new("Tj", vec![Object::String(PdfString::literal(bytes))])),
        None => {
            let win = printcraft_fonts::win_ansi(&text);
            // WinAnsi turns what it can't show into '?'; refuse rather than print the wrong thing.
            let back: String = win.iter().map(|b| char::from_u32(u32::from(*b)).unwrap_or('?')).collect();
            if text.chars().zip(back.chars()).any(|(a, b)| b == '?' && a != '?') {
                return Err(EditError::Invalid(format!("\"{text}\" has characters neither {} nor Helvetica can show", target.base_font)));
            }
            // The size in text space: the current Tf's size.
            let size = font_size_before(&ops, first).unwrap_or(target.size);
            replacement.push(Op::new("Tf", vec![Object::name(std::str::from_utf8(SUBSTITUTE).expect("ascii")), printcraft_content::num(size)]));
            replacement.push(Op::new("Tj", vec![Object::String(PdfString::literal(win))]));
            replacement.push(Op::new("Tf", vec![Object::name(&target.font), printcraft_content::num(size)]));
            substituted = Some("Helvetica".to_string());
            let mut f = Dict::new();
            f.set(b"Type".to_vec(), Object::name("Font"));
            f.set(b"Subtype".to_vec(), Object::name("Type1"));
            f.set(b"BaseFont".to_vec(), Object::name("Helvetica"));
            f.set(b"Encoding".to_vec(), Object::name("WinAnsiEncoding"));
            fonts_res.set(SUBSTITUTE.to_vec(), Object::Dict(f));
        }
    }
    // Rebuild: the line's first operator becomes the replacement, its others go.
    let drop: std::collections::HashSet<usize> = target.ops[1..].iter().copied().collect();
    let mut new_ops = Vec::with_capacity(ops.len() + replacement.len());
    for (i, op) in ops.drain(..).enumerate() {
        if i == first {
            new_ops.append(&mut replacement);
        } else if !drop.contains(&i) {
            new_ops.push(op);
        }
    }
    let mut dict = match &*doc.resolve(&stream_obj) {
        Object::Stream(s) => s.dict.clone(),
        _ => Dict::new(),
    };
    dict.remove(b"Length");
    let new = doc.add(Object::Stream(Stream::flate(dict, &serialize_ops(&new_ops))));
    let contents: Vec<Object> = streams.iter().enumerate().map(|(i, (o, _))| if i == target.stream { Object::Ref(new) } else { o.clone() }).collect();
    let page_ref = p.obj;
    if substituted.is_some() {
        res.set(b"Font".to_vec(), Object::Dict(fonts_res));
    }
    doc.update_dict(page_ref, |d| {
        d.set(b"Contents".to_vec(), if contents.len() == 1 { contents[0].clone() } else { Object::Array(contents) });
        if substituted.is_some() {
            d.set(b"Resources".to_vec(), Object::Dict(res));
        }
    })?;
    Ok(LineEdit { substituted })
}

/// The size of the last `Tf` before operator `at`.
fn font_size_before(ops: &[Op], at: usize) -> Option<f64> {
    ops[..at].iter().rev().find(|o| o.is("Tf")).and_then(|o| o.num(1))
}
