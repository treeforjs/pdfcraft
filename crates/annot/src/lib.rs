//! printcraft-annot — comments (annotations), ISO 32000-2 §12.5, execution plan M5.1–M5.3.
//!
//! Builders for the comment types Acrobat's commenting tools create (sticky note, highlight,
//! underline, strikethrough, squiggly, rectangle, oval, line/arrow, freehand ink, text box),
//! appearance streams for them ([`appearance`]), and the edits a comment goes through: reply,
//! change its text, recolour, move, resize and delete.
//!
//! Addressing: a comment is `(page, index)`, its position in the page's `/Annots` array, which
//! is what `printcraft_render::Annotation::index` reports. Inline annotation dictionaries are
//! promoted to indirect objects when they are edited (replies need a reference to point at).
//!
//! Every edit mutates a `printcraft_cos::Document` (copy-on-write); callers snapshot it first
//! for undo. Keys we do not understand are left alone.

use printcraft_cos::{Dict, Document, ObjRef, Object, PdfString};

pub mod appearance;

#[cfg(test)]
mod tests;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum AnnotError {
    #[error("the document has no page tree")]
    NoPageTree,
    #[error("page {0} does not exist")]
    NoSuchPage(usize),
    #[error("there is no comment {index} on page {page}")]
    NoSuchAnnotation { page: usize, index: usize },
    #[error("{0}")]
    Invalid(String),
    #[error("{0} comments can't be restyled yet (their appearance can't be regenerated)")]
    Unsupported(String),
    #[error("{0}")]
    Cos(#[from] printcraft_cos::CosError),
}

pub type Rgb = [f64; 3];

/// Text markup kinds (§12.5.6.10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Markup {
    Highlight,
    Underline,
    StrikeOut,
    Squiggly,
}

impl Markup {
    fn subtype(self) -> &'static str {
        match self {
            Markup::Highlight => "Highlight",
            Markup::Underline => "Underline",
            Markup::StrikeOut => "StrikeOut",
            Markup::Squiggly => "Squiggly",
        }
    }
}

/// The standard text-annotation icon names (§12.5.6.4, Table 175).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NoteIcon {
    #[default]
    Comment,
    Note,
    Help,
    Insert,
    Key,
    NewParagraph,
    Paragraph,
}

impl NoteIcon {
    pub fn name(self) -> &'static str {
        match self {
            NoteIcon::Comment => "Comment",
            NoteIcon::Note => "Note",
            NoteIcon::Help => "Help",
            NoteIcon::Insert => "Insert",
            NoteIcon::Key => "Key",
            NoteIcon::NewParagraph => "NewParagraph",
            NoteIcon::Paragraph => "Paragraph",
        }
    }

    pub fn from_name(n: &str) -> Option<Self> {
        [Self::Comment, Self::Note, Self::Help, Self::Insert, Self::Key, Self::NewParagraph, Self::Paragraph].into_iter().find(|i| i.name() == n)
    }
}

/// Geometry of a new comment, in PDF user space of its page.
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    /// A sticky note whose icon's top-left corner is at `at`.
    Note {
        at: [f64; 2],
        icon: NoteIcon,
    },
    /// Text markup over quadrilaterals: `[x1 y1 x2 y2 x3 y3 x4 y4]` = top-left, top-right,
    /// bottom-left, bottom-right (the order Acrobat writes, §12.5.6.10).
    TextMarkup {
        kind: Markup,
        quads: Vec<[f64; 8]>,
    },
    Rectangle {
        rect: [f64; 4],
    },
    Oval {
        rect: [f64; 4],
    },
    /// A line, with an open arrowhead at `to` when `arrow` is set.
    Line {
        from: [f64; 2],
        to: [f64; 2],
        arrow: bool,
    },
    /// Freehand strokes (Draw tool).
    Ink {
        strokes: Vec<Vec<[f64; 2]>>,
    },
    /// A text box (FreeText) showing the comment's contents.
    TextBox {
        rect: [f64; 4],
        font_size: f64,
    },
}

impl Shape {
    pub fn subtype(&self) -> &'static str {
        match self {
            Shape::Note { .. } => "Text",
            Shape::TextMarkup { kind, .. } => kind.subtype(),
            Shape::Rectangle { .. } => "Square",
            Shape::Oval { .. } => "Circle",
            Shape::Line { .. } => "Line",
            Shape::Ink { .. } => "Ink",
            Shape::TextBox { .. } => "FreeText",
        }
    }
}

/// How a comment looks.
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    /// Stroke colour (text colour for text boxes, icon colour for notes).
    pub color: Rgb,
    /// 0–1 (`/CA`).
    pub opacity: f64,
    /// Border / stroke width in points (0 = none).
    pub width: f64,
    /// Interior fill for rectangles and ovals (`/IC`), background for text boxes.
    pub fill: Option<Rgb>,
}

impl Default for Style {
    fn default() -> Self {
        Self { color: [1.0, 0.82, 0.0], opacity: 1.0, width: 1.0, fill: None }
    }
}

impl Style {
    /// The default look of each commenting tool.
    pub fn default_for(shape: &Shape) -> Self {
        let (color, width) = match shape {
            Shape::Note { .. } => ([1.0, 0.82, 0.0], 1.0),
            Shape::TextMarkup { kind: Markup::Highlight, .. } => ([1.0, 0.94, 0.0], 1.0),
            Shape::TextMarkup { kind: Markup::Underline, .. } => ([0.0, 0.47, 0.84], 1.0),
            Shape::TextMarkup { kind: Markup::StrikeOut, .. } => ([0.89, 0.13, 0.13], 1.0),
            Shape::TextMarkup { kind: Markup::Squiggly, .. } => ([0.18, 0.62, 0.36], 1.0),
            Shape::Rectangle { .. } | Shape::Oval { .. } | Shape::Line { .. } => ([0.89, 0.13, 0.13], 2.0),
            Shape::Ink { .. } => ([0.0, 0.4, 0.87], 2.0),
            Shape::TextBox { .. } => ([0.0, 0.0, 0.0], 0.0),
        };
        Self { color, opacity: 1.0, width, fill: None }
    }
}

/// A comment to add.
#[derive(Clone, Debug, PartialEq)]
pub struct NewAnnotation {
    /// 0-based page index.
    pub page: usize,
    pub shape: Shape,
    pub style: Style,
    /// The comment text (`/Contents`); the text shown by a text box.
    pub contents: String,
    /// `/T` (shown as the comment's author).
    pub author: String,
}

/// Values stamped onto what an edit creates or changes, supplied by the caller so edits stay
/// deterministic: a PDF date (`D:…`) and a unique id for `/NM`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Meta {
    pub date: Option<String>,
    pub id: String,
}

/// Review states a reply can set (§12.5.6.3, Table 172; Acrobat's "Set status").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewState {
    None,
    Accepted,
    Rejected,
    Cancelled,
    Completed,
}

impl ReviewState {
    pub fn name(self) -> &'static str {
        match self {
            ReviewState::None => "None",
            ReviewState::Accepted => "Accepted",
            ReviewState::Rejected => "Rejected",
            ReviewState::Cancelled => "Cancelled",
            ReviewState::Completed => "Completed",
        }
    }

    pub fn from_name(n: &str) -> Option<Self> {
        [Self::None, Self::Accepted, Self::Rejected, Self::Cancelled, Self::Completed].into_iter().find(|s| s.name().eq_ignore_ascii_case(n))
    }
}

// ── pages and /Annots ───────────────────────────────────────────────────────────────────────

/// The page object of each leaf page, in order.
pub fn page_refs(doc: &Document) -> Result<Vec<ObjRef>, AnnotError> {
    let root = doc.root().ok_or(AnnotError::NoPageTree)?;
    let pages = doc.get(root).as_dict().and_then(|d| d.reference(b"Pages")).ok_or(AnnotError::NoPageTree)?;
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![pages];
    while let Some(node) = stack.pop() {
        if !seen.insert(node) || seen.len() > 1_000_000 {
            continue;
        }
        let obj = doc.get(node);
        let Some(d) = obj.as_dict() else { continue };
        match d.get(b"Kids").map(|k| doc.resolve(k)) {
            Some(kids) if d.name(b"Type") != Some(b"Page") => {
                if let Some(a) = kids.as_array() {
                    stack.extend(a.iter().rev().filter_map(|k| k.as_ref()));
                }
            }
            _ => out.push(node),
        }
    }
    Ok(out)
}

fn page_ref(doc: &Document, page: usize) -> Result<ObjRef, AnnotError> {
    page_refs(doc)?.get(page).copied().ok_or(AnnotError::NoSuchPage(page))
}

/// The entries of a page's `/Annots` (references or inline dictionaries).
fn annots(doc: &Document, page: ObjRef) -> Vec<Object> {
    let obj = doc.get(page);
    let Some(a) = obj.as_dict().and_then(|d| d.get(b"Annots")) else { return Vec::new() };
    doc.resolve(a).as_array().cloned().unwrap_or_default()
}

/// Replace a page's `/Annots`, writing into the shared array object when it is indirect.
fn set_annots(doc: &mut Document, page: ObjRef, list: Vec<Object>) -> Result<(), AnnotError> {
    let existing = doc.get(page).as_dict().and_then(|d| d.reference(b"Annots"));
    match existing {
        Some(r) if doc.get(r).as_array().is_some() => doc.set(r, Object::Array(list)),
        _ => doc.update_dict(page, |d| {
            if list.is_empty() {
                d.remove(b"Annots");
            } else {
                d.set(b"Annots".to_vec(), Object::Array(list));
            }
        })?,
    }
    Ok(())
}

/// The annotation at `(page, index)` as an indirect object (inline dictionaries are promoted).
fn annot_ref(doc: &mut Document, page: usize, index: usize) -> Result<(ObjRef, ObjRef), AnnotError> {
    let p = page_ref(doc, page)?;
    let mut list = annots(doc, p);
    let entry = list.get(index).cloned().ok_or(AnnotError::NoSuchAnnotation { page, index })?;
    let r = match entry {
        Object::Ref(r) if doc.get(r).as_dict().is_some() => r,
        Object::Dict(d) => {
            let r = doc.add(Object::Dict(d));
            list[index] = Object::Ref(r);
            set_annots(doc, p, list)?;
            r
        }
        _ => return Err(AnnotError::NoSuchAnnotation { page, index }),
    };
    Ok((p, r))
}

fn annot_dict(doc: &Document, r: ObjRef) -> Dict {
    doc.get(r).as_dict().cloned().unwrap_or_default()
}

// ── building ────────────────────────────────────────────────────────────────────────────────

/// Annotation flags (§12.5.3).
const FLAG_PRINT: i64 = 4;
const FLAG_NO_ZOOM: i64 = 8;
const FLAG_NO_ROTATE: i64 = 16;

/// Size of a note icon (points, unscaled by zoom).
pub const NOTE_SIZE: f64 = 20.0;

fn num_array(v: &[f64]) -> Object {
    Object::Array(v.iter().map(|x| Object::Real(*x)).collect())
}

fn rgb(c: Rgb) -> Object {
    num_array(&c.map(|x| x.clamp(0.0, 1.0)))
}

fn finite(v: &[f64]) -> bool {
    v.iter().all(|x| x.is_finite() && x.abs() < 1e7)
}

fn normalize(r: [f64; 4]) -> [f64; 4] {
    [r[0].min(r[2]), r[1].min(r[3]), r[0].max(r[2]), r[1].max(r[3])]
}

fn bounds(points: impl Iterator<Item = [f64; 2]>) -> Option<[f64; 4]> {
    points.fold(None, |acc, [x, y]| match acc {
        None => Some([x, y, x, y]),
        Some([a, b, c, d]) => Some([a.min(x), b.min(y), c.max(x), d.max(y)]),
    })
}

fn grow(r: [f64; 4], by: f64) -> [f64; 4] {
    [r[0] - by, r[1] - by, r[2] + by, r[3] + by]
}

/// Validate and compute `/Rect` for a new comment.
fn rect_for(shape: &Shape, style: &Style) -> Result<[f64; 4], AnnotError> {
    let bad = |what: &str| AnnotError::Invalid(format!("invalid {what}"));
    let half = style.width.max(0.0) / 2.0;
    let r = match shape {
        Shape::Note { at, .. } => {
            if !finite(at) {
                return Err(bad("position"));
            }
            [at[0], at[1] - NOTE_SIZE, at[0] + NOTE_SIZE, at[1]]
        }
        Shape::TextMarkup { quads, .. } => {
            if quads.is_empty() || !quads.iter().all(|q| finite(q)) {
                return Err(bad("text area (no quadrilaterals)"));
            }
            bounds(quads.iter().flat_map(|q| q.chunks_exact(2).map(|p| [p[0], p[1]]).collect::<Vec<_>>())).ok_or_else(|| bad("text area"))?
        }
        Shape::Rectangle { rect } | Shape::Oval { rect } | Shape::TextBox { rect, .. } => {
            let r = normalize(*rect);
            if !finite(rect) || r[2] - r[0] < 1.0 || r[3] - r[1] < 1.0 {
                return Err(bad("rectangle (too small)"));
            }
            r
        }
        Shape::Line { from, to, arrow } => {
            if !finite(from) || !finite(to) || (from[0] - to[0]).hypot(from[1] - to[1]) < 1.0 {
                return Err(bad("line (too short)"));
            }
            let pad = half + if *arrow { appearance::arrow_size(style.width) } else { 0.0 };
            grow(bounds([*from, *to].into_iter()).unwrap_or_default(), pad + 1.0)
        }
        Shape::Ink { strokes } => {
            if strokes.iter().all(|s| s.is_empty()) || !strokes.iter().flatten().all(|p| finite(p)) {
                return Err(bad("drawing (no points)"));
            }
            grow(bounds(strokes.iter().flatten().copied()).unwrap_or_default(), half + 1.0)
        }
    };
    Ok(r)
}

fn base_dict(subtype: &str, rect: [f64; 4], page: ObjRef, contents: &str, author: &str, meta: &Meta) -> Dict {
    let mut d = Dict::new();
    d.set(b"Type".to_vec(), Object::name("Annot"));
    d.set(b"Subtype".to_vec(), Object::name(subtype));
    d.set(b"Rect".to_vec(), num_array(&rect));
    d.set(b"Contents".to_vec(), PdfString::text(contents));
    if !author.is_empty() {
        d.set(b"T".to_vec(), PdfString::text(author));
    }
    if let Some(date) = &meta.date {
        d.set(b"M".to_vec(), PdfString::literal(date.as_bytes().to_vec()));
        d.set(b"CreationDate".to_vec(), PdfString::literal(date.as_bytes().to_vec()));
    }
    if !meta.id.is_empty() {
        d.set(b"NM".to_vec(), PdfString::text(&meta.id));
    }
    d.set(b"P".to_vec(), Object::Ref(page));
    d.set(b"F".to_vec(), Object::Int(FLAG_PRINT));
    d
}

/// Acrobat's `/Subj` for each tool (shown as the comment type in other viewers).
fn subject(shape: &Shape) -> &'static str {
    match shape {
        Shape::Note { .. } => "Sticky Note",
        Shape::TextMarkup { kind: Markup::Highlight, .. } => "Highlight",
        Shape::TextMarkup { kind: Markup::Underline, .. } => "Underline",
        Shape::TextMarkup { kind: Markup::StrikeOut, .. } => "Strikethrough",
        Shape::TextMarkup { kind: Markup::Squiggly, .. } => "Squiggly",
        Shape::Rectangle { .. } => "Rectangle",
        Shape::Oval { .. } => "Oval",
        Shape::Line { arrow: true, .. } => "Arrow",
        Shape::Line { .. } => "Line",
        Shape::Ink { .. } => "Pencil",
        Shape::TextBox { .. } => "Text Box",
    }
}

/// Add a comment; returns its index in the page's `/Annots`.
pub fn add_annotation(doc: &mut Document, new: &NewAnnotation, meta: &Meta) -> Result<usize, AnnotError> {
    let page = page_ref(doc, new.page)?;
    let style = &new.style;
    if !finite(&style.color) || !style.opacity.is_finite() || !style.width.is_finite() {
        return Err(AnnotError::Invalid("invalid style".into()));
    }
    let rect = rect_for(&new.shape, style)?;
    let mut d = base_dict(new.shape.subtype(), rect, page, &new.contents, &new.author, meta);
    d.set(b"Subj".to_vec(), PdfString::text(subject(&new.shape)));
    let opacity = style.opacity.clamp(0.0, 1.0);
    if opacity < 1.0 {
        d.set(b"CA".to_vec(), Object::Real(opacity));
    }
    let border = |d: &mut Dict| {
        let mut bs = Dict::new();
        bs.set(b"W".to_vec(), Object::Real(style.width.max(0.0)));
        bs.set(b"S".to_vec(), Object::name("S"));
        d.set(b"BS".to_vec(), Object::Dict(bs));
    };
    let mut popup = None;
    match &new.shape {
        Shape::Note { icon, .. } => {
            d.set(b"C".to_vec(), rgb(style.color));
            d.set(b"Name".to_vec(), Object::name(icon.name()));
            d.set(b"F".to_vec(), Object::Int(FLAG_PRINT | FLAG_NO_ZOOM | FLAG_NO_ROTATE));
            d.set(b"Open".to_vec(), Object::Bool(false));
            popup = Some([rect[2] + 10.0, rect[3] - 120.0, rect[2] + 210.0, rect[3]]);
        }
        Shape::TextMarkup { quads, .. } => {
            d.set(b"C".to_vec(), rgb(style.color));
            d.set(b"QuadPoints".to_vec(), num_array(&quads.concat()));
        }
        Shape::Rectangle { .. } | Shape::Oval { .. } => {
            d.set(b"C".to_vec(), rgb(style.color));
            if let Some(f) = style.fill {
                d.set(b"IC".to_vec(), rgb(f));
            }
            border(&mut d);
        }
        Shape::Line { from, to, arrow } => {
            d.set(b"C".to_vec(), rgb(style.color));
            d.set(b"L".to_vec(), num_array(&[from[0], from[1], to[0], to[1]]));
            if *arrow {
                d.set(b"LE".to_vec(), Object::Array(vec![Object::name("None"), Object::name("OpenArrow")]));
            }
            border(&mut d);
        }
        Shape::Ink { strokes } => {
            d.set(b"C".to_vec(), rgb(style.color));
            let list = strokes.iter().filter(|s| !s.is_empty()).map(|s| num_array(&s.concat())).collect();
            d.set(b"InkList".to_vec(), Object::Array(list));
            border(&mut d);
        }
        Shape::TextBox { font_size, .. } => {
            let size = if font_size.is_finite() && *font_size > 0.0 { font_size.clamp(1.0, 400.0) } else { 12.0 };
            let [r, g, b] = style.color.map(|x| x.clamp(0.0, 1.0));
            d.set(b"DA".to_vec(), PdfString::literal(format!("{} {} {} rg /Helv {} Tf", n(r), n(g), n(b), n(size)).into_bytes()));
            d.set(b"Q".to_vec(), Object::Int(0));
            if let Some(f) = style.fill {
                d.set(b"C".to_vec(), rgb(f));
            }
            border(&mut d);
        }
    }
    let r = doc.add(Object::Dict(d.clone()));
    set_appearance(doc, r)?;
    let mut list = annots(doc, page);
    let index = list.len();
    list.push(Object::Ref(r));
    if let Some(pr) = popup {
        let mut p = Dict::new();
        p.set(b"Type".to_vec(), Object::name("Annot"));
        p.set(b"Subtype".to_vec(), Object::name("Popup"));
        p.set(b"Rect".to_vec(), num_array(&pr));
        p.set(b"Parent".to_vec(), Object::Ref(r));
        p.set(b"Open".to_vec(), Object::Bool(false));
        p.set(b"F".to_vec(), Object::Int(FLAG_PRINT | FLAG_NO_ZOOM | FLAG_NO_ROTATE));
        let pref = doc.add(Object::Dict(p));
        doc.update_dict(r, |d| d.set(b"Popup".to_vec(), Object::Ref(pref)))?;
        list.push(Object::Ref(pref));
    }
    set_annots(doc, page, list)?;
    Ok(index)
}

/// (Re)generate `/AP /N` for the annotation `r` from its dictionary.
fn set_appearance(doc: &mut Document, r: ObjRef) -> Result<(), AnnotError> {
    let d = annot_dict(doc, r);
    let subtype = String::from_utf8_lossy(d.name(b"Subtype").unwrap_or_default()).into_owned();
    let Some(stream) = appearance::build(&d) else { return Err(AnnotError::Unsupported(subtype)) };
    let ap = doc.add(Object::Stream(stream));
    let mut apd = Dict::new();
    apd.set(b"N".to_vec(), Object::Ref(ap));
    doc.update_dict(r, |d| {
        d.set(b"AP".to_vec(), Object::Dict(apd));
        d.remove(b"AS");
    })?;
    Ok(())
}

/// Format a number for content streams and DA strings.
pub(crate) fn n(v: f64) -> String {
    let s = format!("{:.3}", if v.abs() < 5e-4 { 0.0 } else { v });
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

// ── edits ───────────────────────────────────────────────────────────────────────────────────

fn touch(d: &mut Dict, meta: &Meta) {
    if let Some(date) = &meta.date {
        d.set(b"M".to_vec(), PdfString::literal(date.as_bytes().to_vec()));
    }
}

/// Delete a comment together with its pop-up and its replies (and theirs), as Acrobat does.
pub fn delete_annotation(doc: &mut Document, page: usize, index: usize) -> Result<(), AnnotError> {
    let p = page_ref(doc, page)?;
    let list = annots(doc, p);
    let target = list.get(index).cloned().ok_or(AnnotError::NoSuchAnnotation { page, index })?;
    let mut doomed: Vec<ObjRef> = target.as_ref().into_iter().collect();
    // Replies (`/IRT`) and pop-ups (`/Parent`) of anything doomed, to a fixpoint.
    loop {
        let before = doomed.len();
        for e in &list {
            let Some(r) = e.as_ref() else { continue };
            if doomed.contains(&r) {
                continue;
            }
            let obj = doc.get(r);
            let Some(d) = obj.as_dict() else { continue };
            let points_at = |k: &[u8]| d.reference(k).is_some_and(|t| doomed.contains(&t));
            if points_at(b"IRT") || points_at(b"Parent") {
                doomed.push(r);
            }
        }
        for r in doomed.clone() {
            if let Some(pop) = doc.get(r).as_dict().and_then(|d| d.reference(b"Popup"))
                && !doomed.contains(&pop)
            {
                doomed.push(pop);
            }
        }
        if doomed.len() == before {
            break;
        }
    }
    let kept: Vec<Object> =
        list.into_iter().enumerate().filter(|(i, e)| *i != index && !e.as_ref().is_some_and(|r| doomed.contains(&r))).map(|(_, e)| e).collect();
    set_annots(doc, p, kept)
}

/// Change a comment's text. Text boxes are redrawn to show it.
pub fn set_contents(doc: &mut Document, page: usize, index: usize, text: &str, meta: &Meta) -> Result<(), AnnotError> {
    let (_, r) = annot_ref(doc, page, index)?;
    let free_text = annot_dict(doc, r).name(b"Subtype") == Some(b"FreeText");
    doc.update_dict(r, |d| {
        d.set(b"Contents".to_vec(), PdfString::text(text));
        // The rich-text version would contradict the new plain text.
        d.remove(b"RC");
        touch(d, meta);
    })?;
    if free_text {
        set_appearance(doc, r)?;
    }
    Ok(())
}

/// Reply to a comment; returns the reply's index in the page's `/Annots`.
///
/// A reply is a text annotation with `/IRT` pointing at its parent (§12.5.6.2). It gets the
/// parent's rectangle and an empty appearance, so it shows in comment lists but never paints a
/// second icon on the page.
pub fn add_reply(doc: &mut Document, page: usize, index: usize, text: &str, author: &str, meta: &Meta) -> Result<usize, AnnotError> {
    reply(doc, page, index, text, author, meta, None)
}

/// Set a comment's review status (Acrobat: "Set status ▸ Accepted"…). Like Acrobat this adds a
/// state reply (`/State`, `/StateModel /Review`) by `author`; the latest one wins.
pub fn set_review_state(doc: &mut Document, page: usize, index: usize, state: ReviewState, author: &str, meta: &Meta) -> Result<usize, AnnotError> {
    let text = format!("{} set by {}", state.name(), if author.is_empty() { "unknown" } else { author });
    reply(doc, page, index, &text, author, meta, Some(state))
}

fn reply(
    doc: &mut Document,
    page: usize,
    index: usize,
    text: &str,
    author: &str,
    meta: &Meta,
    state: Option<ReviewState>,
) -> Result<usize, AnnotError> {
    let (p, parent) = annot_ref(doc, page, index)?;
    let pd = annot_dict(doc, parent);
    if pd.name(b"Subtype") == Some(b"Popup") {
        return Err(AnnotError::Invalid("pop-ups can't be replied to".into()));
    }
    let rect = pd.get(b"Rect").cloned().unwrap_or_else(|| num_array(&[0.0, 0.0, 0.0, 0.0]));
    let mut d = base_dict("Text", [0.0; 4], p, text, author, meta);
    d.set(b"Rect".to_vec(), rect);
    d.set(b"IRT".to_vec(), Object::Ref(parent));
    d.set(b"F".to_vec(), Object::Int(FLAG_PRINT | FLAG_NO_ZOOM | FLAG_NO_ROTATE));
    d.set(b"Name".to_vec(), Object::name("Comment"));
    if let Some(c) = pd.get(b"C") {
        d.set(b"C".to_vec(), c.clone());
    }
    if let Some(s) = state {
        d.set(b"State".to_vec(), PdfString::text(s.name()));
        d.set(b"StateModel".to_vec(), PdfString::text("Review"));
        d.set(b"Subj".to_vec(), PdfString::text("Status"));
    }
    // An empty form: nothing is drawn for the reply itself.
    let mut fd = Dict::new();
    fd.set(b"Type".to_vec(), Object::name("XObject"));
    fd.set(b"Subtype".to_vec(), Object::name("Form"));
    fd.set(b"BBox".to_vec(), num_array(&[0.0, 0.0, 0.0, 0.0]));
    let ap = doc.add(Object::Stream(printcraft_cos::Stream::from_raw(fd, Vec::new())));
    let mut apd = Dict::new();
    apd.set(b"N".to_vec(), Object::Ref(ap));
    d.set(b"AP".to_vec(), Object::Dict(apd));
    let r = doc.add(Object::Dict(d));
    let mut list = annots(doc, p);
    list.push(Object::Ref(r));
    let i = list.len() - 1;
    set_annots(doc, p, list)?;
    Ok(i)
}

/// Move a comment (and its pop-up) by `(dx, dy)` points. The appearance moves with `/Rect`.
pub fn move_annotation(doc: &mut Document, page: usize, index: usize, dx: f64, dy: f64, meta: &Meta) -> Result<(), AnnotError> {
    if !finite(&[dx, dy]) {
        return Err(AnnotError::Invalid("invalid offset".into()));
    }
    let (_, r) = annot_ref(doc, page, index)?;
    let shift = |o: &Object, every: bool| -> Option<Object> {
        let a = o.as_array()?;
        Some(Object::Array(
            a.iter()
                .enumerate()
                .map(|(i, v)| match v.as_f64() {
                    Some(x) if every || i < 4 => Object::Real(x + if i % 2 == 0 { dx } else { dy }),
                    _ => v.clone(),
                })
                .collect(),
        ))
    };
    let popup = annot_dict(doc, r).reference(b"Popup");
    doc.update_dict(r, |d| {
        for (k, every) in [(&b"Rect"[..], false), (b"QuadPoints", true), (b"L", false), (b"CL", true), (b"Vertices", true)] {
            if let Some(v) = d.get(k).and_then(|o| shift(o, every)) {
                d.set(k.to_vec(), v);
            }
        }
        if let Some(Object::Array(list)) = d.get(b"InkList").cloned() {
            let moved = list.iter().map(|s| shift(s, true).unwrap_or_else(|| s.clone())).collect();
            d.set(b"InkList".to_vec(), Object::Array(moved));
        }
        touch(d, meta);
    })?;
    if let Some(p) = popup
        && doc.get(p).as_dict().is_some()
    {
        doc.update_dict(p, |d| {
            if let Some(v) = d.get(b"Rect").and_then(|o| shift(o, false)) {
                d.set(b"Rect".to_vec(), v);
            }
        })?;
    }
    Ok(())
}

/// Resize a rectangle, oval or text box to `rect`; its appearance is redrawn.
pub fn set_rect(doc: &mut Document, page: usize, index: usize, rect: [f64; 4], meta: &Meta) -> Result<(), AnnotError> {
    let (_, r) = annot_ref(doc, page, index)?;
    let d = annot_dict(doc, r);
    let subtype = String::from_utf8_lossy(d.name(b"Subtype").unwrap_or_default()).into_owned();
    if !matches!(subtype.as_str(), "Square" | "Circle" | "FreeText") {
        return Err(AnnotError::Invalid(format!("{subtype} comments can't be resized")));
    }
    let rect = normalize(rect);
    if !finite(&rect) || rect[2] - rect[0] < 1.0 || rect[3] - rect[1] < 1.0 {
        return Err(AnnotError::Invalid("invalid rectangle (too small)".into()));
    }
    doc.update_dict(r, |d| {
        d.set(b"Rect".to_vec(), num_array(&rect));
        d.remove(b"RD");
        touch(d, meta);
    })?;
    set_appearance(doc, r)
}

/// Change a comment's colour, opacity and/or line width, and redraw it.
pub fn set_style(
    doc: &mut Document,
    page: usize,
    index: usize,
    color: Option<Rgb>,
    opacity: Option<f64>,
    width: Option<f64>,
    meta: &Meta,
) -> Result<(), AnnotError> {
    let (_, r) = annot_ref(doc, page, index)?;
    let d = annot_dict(doc, r);
    let subtype = String::from_utf8_lossy(d.name(b"Subtype").unwrap_or_default()).into_owned();
    // Check before changing anything: a stale appearance would contradict the new style.
    if appearance::build(&d).is_none() {
        return Err(AnnotError::Unsupported(subtype));
    }
    if color.is_some_and(|c| !finite(&c)) || opacity.is_some_and(|o| !o.is_finite()) || width.is_some_and(|w| !w.is_finite()) {
        return Err(AnnotError::Invalid("invalid style".into()));
    }
    let free_text = subtype == "FreeText";
    doc.update_dict(r, |d| {
        if let Some(c) = color {
            if free_text {
                let (_, size) = appearance::parse_da(d);
                let [r, g, b] = c.map(|x| x.clamp(0.0, 1.0));
                d.set(b"DA".to_vec(), PdfString::literal(format!("{} {} {} rg /Helv {} Tf", n(r), n(g), n(b), n(size)).into_bytes()));
            } else {
                d.set(b"C".to_vec(), rgb(c));
            }
        }
        if let Some(o) = opacity {
            let o = o.clamp(0.0, 1.0);
            if o < 1.0 {
                d.set(b"CA".to_vec(), Object::Real(o));
            } else {
                d.remove(b"CA");
            }
        }
        if let Some(w) = width {
            let mut bs = d.get(b"BS").and_then(|b| b.as_dict()).cloned().unwrap_or_default();
            bs.set(b"W".to_vec(), Object::Real(w.max(0.0)));
            d.set(b"BS".to_vec(), Object::Dict(bs));
            d.remove(b"Border");
        }
        touch(d, meta);
    })?;
    set_appearance(doc, r)
}
