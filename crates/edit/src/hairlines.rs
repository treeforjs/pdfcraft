//! Fix Hairlines (PDF: Print Production ▸ Fix Hairlines): strokes thinner than a threshold
//! print as near-invisible lines (a `0 w` line is one device pixel wide), so they are thickened.
//!
//! Widths are measured as printed, in points: the line width (`w`, or `/LW` from an ExtGState)
//! times the scale of the current transformation matrix (the square root of its determinant)
//! and the page's `/UserUnit`. A stroke found too thin gets `<width> w` before its path starts
//! and the old width back after its painting operator, so the rest of the content draws as it
//! did and no operator lands inside a path object. Form XObjects the page draws are fixed too,
//! at the scale of their first use. Stroked text, Type 3 glyphs and patterns are left alone.

use std::collections::{HashMap, HashSet};

use pdfcraft_content::{Matrix, Op, Pieces, num};
use pdfcraft_cos::{Dict, Document, ObjRef, Object, Stream};

use crate::text::{rewritten_contents, splice};
use crate::{EditError, check, page_list};

/// Form XObjects drawn inside form XObjects, at most this deep.
const MAX_DEPTH: usize = 12;

/// What counts as a hairline and how wide it becomes, in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hairlines {
    /// Strokes narrower than this, as printed, are hairlines.
    pub thinner_than: f64,
    /// Their new printed width.
    pub replace_with: f64,
}

impl Default for Hairlines {
    /// A quarter point, the usual thinnest line a press holds.
    fn default() -> Self {
        Self { thinner_than: 0.25, replace_with: 0.25 }
    }
}

/// Thicken hairlines on `pages` (0-based). Returns how many strokes were changed.
pub fn fix_hairlines(doc: &mut Document, pages: &[usize], settings: &Hairlines) -> Result<usize, EditError> {
    let Hairlines { thinner_than, replace_with } = *settings;
    if !(thinner_than.is_finite()
        && replace_with.is_finite()
        && thinner_than > 0.0
        && thinner_than <= 72.0
        && replace_with > 0.0
        && replace_with <= 72.0)
    {
        return Err(EditError::Invalid("hairline widths must be between 0 and 72 points".into()));
    }
    let all = page_list(doc);
    check(pages, all.len())?;
    let mut fixed = 0;
    let mut seen = HashSet::new();
    for &i in pages {
        let Some(page) = all.get(i) else { continue };
        let unit = page.dict.get(b"UserUnit").map(|u| doc.resolve(u)).and_then(|u| u.as_f64()).filter(|u| u.is_finite() && *u > 0.0).unwrap_or(1.0);
        let res = resources(doc, page.dict.get(b"Resources"));
        let streams = crate::text::content_streams(doc, &page.dict);
        let pieces = Pieces::join(&streams.iter().map(|(_, d)| d.as_slice()).collect::<Vec<_>>());
        let ops = pieces.parse().ops;
        let start = Gs { ctm: Matrix([unit, 0.0, 0.0, unit, 0.0, 0.0]), width: 1.0 };
        let plan = plan(doc, &ops, &res, start, settings, &mut seen, 0, &mut fixed);
        if plan.is_empty() {
            continue;
        }
        let new = splice(&pieces, &ops, |k| (plan.get(&k).cloned().unwrap_or_default(), true));
        let contents = rewritten_contents(doc, &streams, new);
        doc.update_dict(page.obj, |d| d.set(b"Contents".to_vec(), contents))?;
    }
    Ok(fixed)
}

#[derive(Clone, Copy)]
struct Gs {
    ctm: Matrix,
    width: f64,
}

fn resources(doc: &Document, o: Option<&Object>) -> Dict {
    o.map(|r| doc.resolve(r)).and_then(|r| r.as_dict().cloned()).unwrap_or_default()
}

fn sub(doc: &Document, res: &Dict, key: &[u8], name: &[u8]) -> Option<Object> {
    let group = doc.resolve(res.get(key)?);
    group.as_dict()?.get(name).cloned()
}

fn width_op(w: f64) -> Op {
    Op::new("w", vec![num((w * 1e4).round() / 1e4)])
}

const STROKES: [&str; 6] = ["S", "s", "B", "B*", "b", "b*"];
const PATH: [&str; 7] = ["m", "l", "c", "v", "y", "h", "re"];
const PAINTS: [&str; 10] = ["S", "s", "f", "F", "f*", "B", "B*", "b", "b*", "n"];

/// Insertions for one stream: operator index → operators to put before it. Form XObjects it
/// draws are rewritten in place (each once).
#[allow(clippy::too_many_arguments)]
fn plan(
    doc: &mut Document,
    ops: &[Op],
    res: &Dict,
    start: Gs,
    settings: &Hairlines,
    seen: &mut HashSet<ObjRef>,
    depth: usize,
    fixed: &mut usize,
) -> HashMap<usize, Vec<Op>> {
    let mut out: HashMap<usize, Vec<Op>> = HashMap::new();
    let mut gs = start;
    let mut stack: Vec<Gs> = Vec::new();
    let mut path_start: Option<usize> = None;
    for (i, op) in ops.iter().enumerate() {
        let k = std::str::from_utf8(&op.op).unwrap_or("");
        match k {
            "q" => {
                if stack.len() < 4096 {
                    stack.push(gs);
                }
            }
            "Q" => gs = stack.pop().unwrap_or(start),
            "cm" => {
                if let Some(m) = Matrix::from_operands(&op.operands).filter(|m| m.0.iter().all(|v| v.is_finite())) {
                    gs.ctm = m.then(&gs.ctm);
                }
            }
            "w" => {
                if let Some(w) = op.num(0).filter(|w| w.is_finite() && *w >= 0.0) {
                    gs.width = w;
                }
            }
            "gs" => {
                let lw = op
                    .name(0)
                    .and_then(|n| sub(doc, res, b"ExtGState", n))
                    .and_then(|g| doc.dict(&g))
                    .and_then(|g| g.get(b"LW").and_then(Object::as_f64));
                if let Some(w) = lw.filter(|w| w.is_finite() && *w >= 0.0) {
                    gs.width = w;
                }
            }
            "Do" => {
                if depth < MAX_DEPTH
                    && let Some(Object::Ref(r)) = op.name(0).and_then(|n| sub(doc, res, b"XObject", n))
                    && seen.insert(r)
                {
                    fix_form(doc, r, gs, res, settings, seen, depth + 1, fixed);
                }
            }
            _ if PATH.contains(&k) => {
                path_start.get_or_insert(i);
            }
            _ if PAINTS.contains(&k) => {
                let Some(begin) = path_start.take() else { continue };
                if !STROKES.contains(&k) {
                    continue;
                }
                let [a, b, c, d, ..] = gs.ctm.0;
                let scale = (a * d - b * c).abs().sqrt();
                if !(scale.is_finite() && scale > 1e-9) || gs.width * scale >= settings.thinner_than - 1e-6 {
                    continue;
                }
                out.entry(begin).or_default().push(width_op(settings.replace_with / scale));
                if i + 1 < ops.len() {
                    out.entry(i + 1).or_default().push(width_op(gs.width));
                }
                *fixed += 1;
            }
            _ => {}
        }
    }
    out
}

/// Fix the hairlines of a form XObject drawn with graphics state `gs` (its `/Matrix` applied).
/// A form without resources of its own uses its caller's (PDF 1.1 style).
#[allow(clippy::too_many_arguments)]
fn fix_form(doc: &mut Document, r: ObjRef, gs: Gs, caller: &Dict, settings: &Hairlines, seen: &mut HashSet<ObjRef>, depth: usize, fixed: &mut usize) {
    let obj = doc.get(r);
    let Object::Stream(s) = &*obj else { return };
    if s.dict.name(b"Subtype") != Some(b"Form".as_slice()) {
        return;
    }
    let Ok(data) = s.decoded() else { return };
    let dict = s.dict.clone();
    let matrix = dict.get(b"Matrix").map(|m| doc.resolve(m)).and_then(|m| m.as_array().and_then(|a| Matrix::from_operands(a))).unwrap_or_default();
    let res = if dict.get(b"Resources").is_some() { resources(doc, dict.get(b"Resources")) } else { caller.clone() };
    let pieces = Pieces::join(&[data.as_slice()]);
    let ops = pieces.parse().ops;
    let inner = Gs { ctm: matrix.then(&gs.ctm), width: gs.width };
    let plan = plan(doc, &ops, &res, inner, settings, seen, depth, fixed);
    if plan.is_empty() {
        return;
    }
    let new = splice(&pieces, &ops, |k| (plan.get(&k).cloned().unwrap_or_default(), true));
    let Some(data) = new.into_iter().next() else { return };
    let mut dict = dict;
    dict.remove(b"Length");
    doc.set(r, Object::Stream(Stream::flate(dict, &data)));
}
