//! printcraft-organize — page and document-structure edits (L4).
//!
//! Every operation mutates a `printcraft_cos::Document` (copy-on-write), so callers snapshot the
//! document before an edit for undo. Operations never drop data they do not understand.
//!
//! Page-tree strategy: before restructuring, inheritable page attributes (`Resources`,
//! `MediaBox`, `CropBox`, `Rotate` — ISO 32000-2 §7.7.3.4) are copied onto each page, then the
//! tree is rebuilt as a single flat `/Pages` node. Intermediate nodes become unreachable (a full
//! save drops them; an incremental save leaves them untouched). Flat trees are valid for any
//! page count and are what most producers write for small documents.

use printcraft_cos::{Dict, Document, ObjRef, Object, PdfString};

mod dedupe;
mod import;

pub use dedupe::dedupe_resources;
pub use import::{SplitBy, combine, extract_pages, import_pages, split, split_ranges};

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum OrganizeError {
    #[error("the document has no page tree")]
    NoPageTree,
    #[error("page {0} does not exist")]
    NoSuchPage(usize),
    #[error("a document must keep at least one page")]
    WouldRemoveAllPages,
    #[error("{0}")]
    Cos(#[from] printcraft_cos::CosError),
}

const INHERITABLE: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

/// A leaf page in document order.
#[derive(Clone, Debug, PartialEq)]
pub struct PageRef {
    pub obj: ObjRef,
}

fn pages_root(doc: &Document) -> Result<ObjRef, OrganizeError> {
    let root = doc.root().ok_or(OrganizeError::NoPageTree)?;
    let catalog = doc.get(root);
    catalog.as_dict().and_then(|d| d.reference(b"Pages")).ok_or(OrganizeError::NoPageTree)
}

/// All leaf pages in order, with their effective inherited attributes.
fn walk(doc: &Document) -> Result<Vec<(ObjRef, Dict)>, OrganizeError> {
    let root = pages_root(doc)?;
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // (node, inherited attributes from ancestors)
    let mut stack: Vec<(ObjRef, Dict)> = vec![(root, Dict::new())];
    while let Some((node, inherited)) = stack.pop() {
        if !seen.insert(node) || seen.len() > 1_000_000 {
            continue; // cycles in broken trees
        }
        let obj = doc.get(node);
        let Some(d) = obj.as_dict() else { continue };
        let mut attrs = inherited.clone();
        for k in INHERITABLE {
            if let Some(v) = d.get(k) {
                attrs.set(k.to_vec(), v.clone());
            }
        }
        let is_pages = d.name(b"Type") == Some(b"Pages") || (d.contains(b"Kids") && d.name(b"Type") != Some(b"Page"));
        if is_pages {
            let kids = d.get(b"Kids").map(|k| doc.resolve(k)).and_then(|k| k.as_array().cloned()).unwrap_or_default();
            for k in kids.iter().rev() {
                if let Some(r) = k.as_ref() {
                    stack.push((r, attrs.clone()));
                }
            }
        } else {
            out.push((node, attrs));
        }
    }
    Ok(out)
}

/// Leaf pages in document order.
pub fn pages(doc: &Document) -> Result<Vec<PageRef>, OrganizeError> {
    Ok(walk(doc)?.into_iter().map(|(obj, _)| PageRef { obj }).collect())
}

pub fn page_count(doc: &Document) -> Result<usize, OrganizeError> {
    Ok(walk(doc)?.len())
}

/// Rebuild the page tree as one flat node with `order` as its kids (attributes pushed down).
fn rebuild(doc: &mut Document, order: &[(ObjRef, Dict)]) -> Result<(), OrganizeError> {
    let root = pages_root(doc)?;
    for (page, inherited) in order {
        doc.update_dict(*page, |d| {
            for k in INHERITABLE {
                if !d.contains(k)
                    && let Some(v) = inherited.get(k)
                {
                    d.set(k.to_vec(), v.clone());
                }
            }
            d.set(b"Parent".to_vec(), Object::Ref(root));
        })?;
    }
    doc.update_dict(root, |d| {
        d.set(b"Type".to_vec(), Object::name("Pages"));
        d.set(b"Kids".to_vec(), Object::Array(order.iter().map(|(r, _)| Object::Ref(*r)).collect()));
        d.set(b"Count".to_vec(), Object::Int(order.len() as i64));
        // Attributes now live on the pages; leaving them here would be harmless but misleading.
        for k in INHERITABLE {
            d.remove(k);
        }
        d.remove(b"Parent");
    })?;
    Ok(())
}

fn check(indices: &[usize], n: usize) -> Result<(), OrganizeError> {
    match indices.iter().find(|i| **i >= n) {
        Some(i) => Err(OrganizeError::NoSuchPage(*i)),
        None => Ok(()),
    }
}

/// Rotate pages by a multiple of 90° (positive = clockwise), adjusting `/Rotate` (§7.7.3.3).
pub fn rotate_pages(doc: &mut Document, indices: &[usize], degrees: i64) -> Result<(), OrganizeError> {
    let all = walk(doc)?;
    check(indices, all.len())?;
    let delta = (degrees / 90) * 90;
    for &i in indices {
        let (page, inherited) = &all[i];
        let current = doc.get(*page).as_dict().and_then(|d| d.int(b"Rotate")).or_else(|| inherited.int(b"Rotate")).unwrap_or(0);
        let new = (current + delta).rem_euclid(360);
        doc.update_dict(*page, |d| d.set(b"Rotate".to_vec(), Object::Int(new)))?;
    }
    Ok(())
}

/// Delete pages. Refuses to delete every page.
pub fn delete_pages(doc: &mut Document, indices: &[usize]) -> Result<(), OrganizeError> {
    let all = walk(doc)?;
    check(indices, all.len())?;
    let keep: Vec<(ObjRef, Dict)> = all.iter().enumerate().filter(|(i, _)| !indices.contains(i)).map(|(_, p)| p.clone()).collect();
    if keep.is_empty() {
        return Err(OrganizeError::WouldRemoveAllPages);
    }
    rebuild(doc, &keep)
}

/// Move the pages at `indices` (kept in their relative order) so they start at position `to`
/// in the resulting document.
pub fn move_pages(doc: &mut Document, indices: &[usize], to: usize) -> Result<(), OrganizeError> {
    let all = walk(doc)?;
    check(indices, all.len())?;
    let mut sorted = indices.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let moving: Vec<(ObjRef, Dict)> = sorted.iter().map(|i| all[*i].clone()).collect();
    let mut rest: Vec<(ObjRef, Dict)> = all.iter().enumerate().filter(|(i, _)| !sorted.contains(i)).map(|(_, p)| p.clone()).collect();
    let at = to.min(rest.len());
    rest.splice(at..at, moving);
    rebuild(doc, &rest)
}

/// Insert a blank page of `width × height` points at position `at` (0 = before the first page).
pub fn insert_blank_page(doc: &mut Document, at: usize, width: f64, height: f64) -> Result<ObjRef, OrganizeError> {
    let mut all = walk(doc)?;
    let root = pages_root(doc)?;
    let mut page = Dict::new();
    page.set(b"Type".to_vec(), Object::name("Page"));
    page.set(b"Parent".to_vec(), Object::Ref(root));
    page.set(b"MediaBox".to_vec(), Object::Array(vec![0.into(), 0.into(), Object::Real(width), Object::Real(height)]));
    page.set(b"Resources".to_vec(), Object::Dict(Dict::new()));
    let r = doc.add(page);
    let at = at.min(all.len());
    all.insert(at, (r, Dict::new()));
    rebuild(doc, &all)?;
    Ok(r)
}

/// Document information keys editable in Document Properties ▸ Description.
pub const INFO_KEYS: [&str; 4] = ["Title", "Author", "Subject", "Keywords"];

/// Read a document-information entry as text.
pub fn info(doc: &Document, key: &str) -> Option<String> {
    let info = doc.trailer().get(b"Info").map(|o| doc.resolve(o))?;
    info.as_dict()?.get(key.as_bytes()).and_then(|v| doc.resolve(v).as_string().map(|s| s.to_text()))
}

/// Set (or clear, with an empty value) a document-information entry.
///
/// Note: documents with XMP metadata also carry these values in the XMP packet; synchronising
/// XMP arrives with the `model` crate (M2) — viewers that prefer XMP may still show old values.
pub fn set_info(doc: &mut Document, key: &str, value: &str) -> Result<(), OrganizeError> {
    let value = value.trim();
    let entry = (!value.is_empty()).then(|| Object::String(PdfString::text(value)));
    match doc.trailer().get(b"Info").cloned() {
        Some(Object::Ref(r)) if doc.get(r).as_dict().is_some() => {
            doc.update_dict(r, |d| match entry {
                Some(v) => d.set(key.as_bytes().to_vec(), v),
                None => {
                    d.remove(key.as_bytes());
                }
            })?;
        }
        _ => {
            let Some(v) = entry else { return Ok(()) };
            let mut d = match doc.trailer().get(b"Info") {
                Some(Object::Dict(d)) => d.clone(),
                _ => Dict::new(),
            };
            d.set(key.as_bytes().to_vec(), v);
            let r = doc.add(d);
            doc.trailer_mut().set(b"Info".to_vec(), Object::Ref(r));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
