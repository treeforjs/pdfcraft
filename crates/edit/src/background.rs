//! What is left of a page without its text and chosen images: the "background" Export to
//! PowerPoint renders under the editable text boxes and pictures it places on a slide.

use std::collections::HashSet;

use pdfcraft_content::{Op, parse, serialize_ops};
use pdfcraft_cos::{Dict, Document, ObjRef, Object, Stream};

use crate::EditError;
use crate::text::{MAX_FORM_DEPTH, MAX_FORM_VISITS};

/// Text-showing operators (ISO 32000-2 §9.4.3).
const SHOW: [&[u8]; 4] = [b"Tj", b"TJ", b"'", b"\""];

/// Remove from page `page` (0-based) every text-showing operator and every `Do` of an image
/// XObject in `images`, in its own content and in the form XObjects it draws (nested forms
/// included, to the same depth and visit limits as reading the page's text). Everything else
/// (vector paths, shadings, inline images, other images, annotations) stays.
///
/// Forms are shared between pages, so `done` carries the forms already rewritten from one
/// call to the next: pass the same set for every page of one document copy. This edits `doc`;
/// it is meant for a scratch copy that is rendered and thrown away.
pub fn strip_text_and_images(doc: &mut Document, page: usize, images: &HashSet<ObjRef>, done: &mut HashSet<ObjRef>) -> Result<(), EditError> {
    let p = pdfcraft_model::pages(doc).into_iter().nth(page).ok_or(EditError::NoSuchPage(page))?;
    let resources = p.dict.get(b"Resources").map(|r| doc.resolve(r)).and_then(|r| r.as_dict().cloned()).unwrap_or_default();
    // The page's streams are one stream in pieces (§7.8.2): join them before parsing.
    let list: Vec<Object> = match p.dict.get(b"Contents") {
        None => Vec::new(),
        Some(c) => match &*doc.resolve(c) {
            Object::Array(a) => a.clone(),
            _ => vec![c.clone()],
        },
    };
    let mut data = Vec::new();
    for o in &list {
        if let Object::Stream(s) = &*doc.resolve(o) {
            data.extend(s.decoded()?);
            data.push(b'\n');
        }
    }
    let kept = filter(doc, &data, &resources, images);
    let contents = doc.add(Object::Stream(Stream::from_raw(Dict::new(), kept)));
    doc.update_dict(p.obj, |d| d.set(b"Contents".to_vec(), Object::Ref(contents)))?;
    let mut visits = 0usize;
    forms(doc, &resources, images, done, 0, &mut visits);
    Ok(())
}

/// `data` without its text-showing operators and its `Do`s of `images`.
fn filter(doc: &Document, data: &[u8], resources: &Dict, images: &HashSet<ObjRef>) -> Vec<u8> {
    let xobjects = resources.get(b"XObject").map(|x| doc.resolve(x)).and_then(|x| x.as_dict().cloned()).unwrap_or_default();
    let drop = |op: &Op| {
        if SHOW.contains(&op.op.as_slice()) {
            return true;
        }
        op.is("Do") && op.name(0).and_then(|n| xobjects.get(n)).and_then(Object::as_ref).is_some_and(|r| images.contains(&r))
    };
    let ops: Vec<Op> = parse(data).ops.into_iter().filter(|op| !drop(op)).collect();
    serialize_ops(&ops)
}

/// Rewrite the form XObjects `resources` names (and the forms they draw) the same way.
fn forms(doc: &mut Document, resources: &Dict, images: &HashSet<ObjRef>, done: &mut HashSet<ObjRef>, depth: usize, visits: &mut usize) {
    if depth >= MAX_FORM_DEPTH {
        return;
    }
    let xobjects = resources.get(b"XObject").map(|x| doc.resolve(x)).and_then(|x| x.as_dict().cloned()).unwrap_or_default();
    for (_, o) in xobjects.iter() {
        let Some(r) = o.as_ref() else { continue };
        if *visits >= MAX_FORM_VISITS || !done.insert(r) {
            continue;
        }
        *visits += 1;
        let obj = doc.get(r);
        let Object::Stream(s) = &*obj else { continue };
        if s.dict.name(b"Subtype") != Some(b"Form") {
            continue;
        }
        let Ok(data) = s.decoded() else { continue };
        // A form without its own resources uses the ones it was drawn with.
        let own = s.dict.get(b"Resources").map(|x| doc.resolve(x)).and_then(|x| x.as_dict().cloned()).unwrap_or_else(|| resources.clone());
        let kept = filter(doc, &data, &own, images);
        let mut dict = s.dict.clone();
        for k in [&b"Filter"[..], b"DecodeParms", b"Length", b"DL"] {
            dict.remove(k);
        }
        doc.set(r, Object::Stream(Stream::from_raw(dict, kept)));
        forms(doc, &own, images, done, depth + 1, visits);
    }
}
