//! Comment pop-ups (ISO 32000-2 §12.5.6.14): the note window a markup annotation's text is
//! shown in. A pop-up is its own `/Subtype /Popup` annotation, linked both ways (`/Popup` on the
//! comment, `/Parent` on the pop-up), with its own `/Rect` and an `/Open` flag. Sticky notes
//! also carry `/Open` themselves, which is kept in step.
//!
//! Pop-ups keep their size on screen at every zoom and stay upright on rotated pages (`/F`
//! NoZoom | NoRotate), so their rectangle is read as a top-left corner (`[x0, y1]` in user
//! space) and a width and height in points.

use pdfcraft_cos::{Dict, Document, ObjRef, Object};

use crate::{
    AnnotError, FLAG_NO_ROTATE, FLAG_NO_ZOOM, FLAG_PRINT, annot_dict, annot_ref, annots, finite, is_comment_subtype, normalize, num_array, set_annots,
};

/// Smallest pop-up side, in points (smaller ones can't show a title bar).
pub const MIN_POPUP_SIDE: f64 = 24.0;
/// Largest pop-up side, in points.
pub const MAX_POPUP_SIDE: f64 = 2000.0;
/// Size of a new pop-up, in points (Acrobat's default note window is about this size).
pub const DEFAULT_POPUP_SIZE: [f64; 2] = [200.0, 120.0];

/// A comment's pop-up as stored in the file.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PopupState {
    /// `/Rect` in user space, normalised. A missing or unusable one is replaced by
    /// [`default_popup_rect`] next to the comment.
    pub rect: [f64; 4],
    /// `/Open`: shown when the document opens.
    pub open: bool,
}

/// Where a new pop-up goes: to the right of the comment, top edges aligned (as for new notes).
pub fn default_popup_rect(parent: [f64; 4]) -> [f64; 4] {
    let p = normalize(parent);
    let (x, top) = if finite(&p) { (p[2] + 10.0, p[3]) } else { (0.0, DEFAULT_POPUP_SIZE[1]) };
    [x, top - DEFAULT_POPUP_SIZE[1], x + DEFAULT_POPUP_SIZE[0], top]
}

/// A usable pop-up rectangle: finite, not absurdly far away, and at least a point on each side.
fn usable(r: [f64; 4]) -> Option<[f64; 4]> {
    let r = normalize(r);
    (finite(&r) && r[2] - r[0] >= 1.0 && r[3] - r[1] >= 1.0).then_some(r)
}

fn rect_of(doc: &Document, d: &Dict) -> Option<[f64; 4]> {
    let o = doc.resolve(d.get(b"Rect")?);
    let a = o.as_array()?;
    if a.len() != 4 {
        return None;
    }
    let mut r = [0.0; 4];
    for (slot, v) in r.iter_mut().zip(a.iter()) {
        *slot = doc.resolve(v).as_f64()?;
    }
    usable(r)
}

fn open_of(doc: &Document, d: &Dict) -> bool {
    d.get(b"Open").is_some_and(|o| matches!(*doc.resolve(o), Object::Bool(true)))
}

/// The pop-up object a comment dictionary points at, when it really is a pop-up.
fn popup_ref(doc: &Document, d: &Dict) -> Option<ObjRef> {
    let r = d.reference(b"Popup")?;
    doc.get(r).as_dict().is_some_and(|p| p.name(b"Subtype") == Some(b"Popup")).then_some(r)
}

/// The pop-up of the comment dictionary `d` (no pop-up, or a `/Popup` that isn't one: `None`).
pub fn popup_of_dict(doc: &Document, d: &Dict) -> Option<PopupState> {
    let r = popup_ref(doc, d)?;
    let obj = doc.get(r);
    let p = obj.as_dict()?;
    let parent = rect_of(doc, d).unwrap_or([0.0; 4]);
    Some(PopupState { rect: rect_of(doc, p).unwrap_or_else(|| default_popup_rect(parent)), open: open_of(doc, p) })
}

/// The pop-up of the comment at `(page, index)`.
pub fn popup(doc: &Document, page: usize, index: usize) -> Option<PopupState> {
    let p = crate::page_ref(doc, page).ok()?;
    let entry = annots(doc, p).get(index).cloned()?;
    let obj = doc.resolve(&entry);
    popup_of_dict(doc, obj.as_dict()?)
}

/// Whether a comment of this subtype can have a pop-up: markup annotations other than text
/// boxes, which show their text on the page.
pub fn has_popup_subtype(subtype: &[u8]) -> bool {
    let s = String::from_utf8_lossy(subtype);
    is_comment_subtype(&s) && !matches!(s.as_ref(), "FreeText" | "Sound")
}

/// Check a pop-up rectangle given by a caller: finite and between [`MIN_POPUP_SIDE`] and
/// [`MAX_POPUP_SIDE`] points on each side.
pub fn check_popup_rect(r: [f64; 4]) -> Result<[f64; 4], AnnotError> {
    let r = normalize(r);
    if !finite(&r) {
        return Err(AnnotError::Invalid("invalid pop-up rectangle".into()));
    }
    let (w, h) = (r[2] - r[0], r[3] - r[1]);
    if w < MIN_POPUP_SIDE || h < MIN_POPUP_SIDE {
        return Err(AnnotError::Invalid(format!("a pop-up must be at least {MIN_POPUP_SIDE} points wide and high")));
    }
    if w > MAX_POPUP_SIDE || h > MAX_POPUP_SIDE {
        return Err(AnnotError::Invalid(format!("a pop-up can be at most {MAX_POPUP_SIDE} points wide and high")));
    }
    Ok(r)
}

/// Open or close a comment's pop-up and/or give it a new rectangle (`rect`, user space). A
/// comment without a pop-up gets one (next to it unless `rect` says where), which is added to
/// the page's `/Annots`; closing a comment that has none changes nothing. Sticky notes' own
/// `/Open` follows the pop-up's. Replies show in their comment's pop-up and have none.
pub fn set_popup(doc: &mut Document, page: usize, index: usize, open: Option<bool>, rect: Option<[f64; 4]>) -> Result<(), AnnotError> {
    if open.is_none() && rect.is_none() {
        return Err(AnnotError::Invalid("nothing to change: open or close the pop-up, or give it a rectangle".into()));
    }
    let rect = rect.map(check_popup_rect).transpose()?;
    let (p, r) = annot_ref(doc, page, index)?;
    let d = annot_dict(doc, r);
    let subtype = d.name(b"Subtype").unwrap_or_default().to_vec();
    if !has_popup_subtype(&subtype) {
        return Err(AnnotError::Invalid(format!("{} annotations have no pop-up", String::from_utf8_lossy(&subtype))));
    }
    if d.contains(b"IRT") {
        return Err(AnnotError::Invalid("replies show in their comment's pop-up".into()));
    }
    let pop = match popup_ref(doc, &d) {
        Some(pop) => pop,
        None if open != Some(true) && rect.is_none() => return Ok(()),
        None => {
            let parent = rect_of(doc, &d).unwrap_or([0.0; 4]);
            let mut pd = Dict::new();
            pd.set(b"Type".to_vec(), Object::name("Annot"));
            pd.set(b"Subtype".to_vec(), Object::name("Popup"));
            pd.set(b"Rect".to_vec(), num_array(&rect.unwrap_or_else(|| default_popup_rect(parent))));
            pd.set(b"Parent".to_vec(), Object::Ref(r));
            pd.set(b"P".to_vec(), Object::Ref(p));
            pd.set(b"Open".to_vec(), Object::Bool(false));
            pd.set(b"F".to_vec(), Object::Int(FLAG_PRINT | FLAG_NO_ZOOM | FLAG_NO_ROTATE));
            let pop = doc.add(Object::Dict(pd));
            doc.update_dict(r, |d| d.set(b"Popup".to_vec(), Object::Ref(pop)))?;
            let mut list = annots(doc, p);
            list.push(Object::Ref(pop));
            set_annots(doc, p, list)?;
            pop
        }
    };
    // A pop-up whose rectangle can't be used gets a usable one when it is opened.
    let current = doc.get(pop).as_dict().and_then(|pd| rect_of(doc, pd));
    let parent = rect_of(doc, &d).unwrap_or([0.0; 4]);
    let new_rect = rect.or_else(|| current.is_none().then(|| default_popup_rect(parent)));
    doc.update_dict(pop, |pd| {
        if let Some(o) = open {
            pd.set(b"Open".to_vec(), Object::Bool(o));
        }
        if let Some(nr) = new_rect {
            pd.set(b"Rect".to_vec(), num_array(&nr));
        }
        if pd.reference(b"Parent").is_none() {
            pd.set(b"Parent".to_vec(), Object::Ref(r));
        }
    })?;
    if let Some(o) = open
        && subtype == b"Text"
    {
        doc.update_dict(r, |d| d.set(b"Open".to_vec(), Object::Bool(o)))?;
    }
    Ok(())
}

/// The comments on `page` (every page when `None`) whose pop-up is not already `open`: the
/// targets of Open All Pop-ups / Close All Pop-ups. Closing lists only comments that have a
/// pop-up; opening also lists notes without one (Acrobat gives every note a pop-up).
pub fn popups_to_toggle(doc: &Document, page: Option<usize>, open: bool) -> Vec<(usize, usize)> {
    let Ok(pages) = crate::page_refs(doc) else { return Vec::new() };
    let mut out = Vec::new();
    for (pi, p) in pages.iter().enumerate() {
        if page.is_some_and(|only| only != pi) {
            continue;
        }
        for (index, entry) in annots(doc, *p).iter().enumerate() {
            let obj = doc.resolve(entry);
            let Some(d) = obj.as_dict() else { continue };
            if d.contains(b"IRT") || !has_popup_subtype(d.name(b"Subtype").unwrap_or_default()) {
                continue;
            }
            match popup_of_dict(doc, d) {
                Some(s) if s.open != open => out.push((pi, index)),
                None if open && d.name(b"Subtype") == Some(b"Text") => out.push((pi, index)),
                _ => {}
            }
        }
    }
    out
}
