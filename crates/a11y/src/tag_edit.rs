//! Tags panel ▸ New Tag and Delete Tag: add an empty structure element, or take one out of the
//! tree with its kids moving up into its parent in its place (so the content it held stays
//! tagged and in the same reading order).
//!
//! Both are all or nothing: the tree is read with the bounded, cycle-safe [`tags`] walk, a
//! truncated tree is refused, and the work is done on a copy that replaces the document only
//! when everything succeeded.

use pdfcraft_cos::{Dict, Document, ObjRef, Object};

use crate::keep::{insert_kid, kid_list, parent_tree, put_number, rebuild_parent_tree, root_ref};
use crate::tags::{TagError, TagKid, allowed_types, tags};

fn invalid(s: impl Into<String>) -> TagError {
    TagError::Invalid(s.into())
}

/// What [`delete_element`] did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeleteResult {
    /// Kids (elements, marked content and object references) moved up into the parent.
    pub lifted: usize,
    /// The element that holds them now (`None`: the structure tree root).
    pub parent: Option<ObjRef>,
    /// Pages whose parent-tree entries were rebuilt (0-based).
    pub pages: Vec<usize>,
}

/// New Tag: an empty element of type `ty` (a standard type or one the role map maps to one) as
/// the last kid of `parent` (`None`: a new top-level element). Returns the new element.
pub fn new_element(doc: &mut Document, parent: Option<ObjRef>, ty: &str) -> Result<ObjRef, TagError> {
    let tree = tags(doc);
    if !tree.exists {
        return Err(TagError::Untagged);
    }
    if tree.truncated {
        return Err(invalid("the structure tree is too large or too deeply nested to edit safely"));
    }
    let root = root_ref(doc)?;
    if let Some(p) = parent
        && tree.find(p).is_none()
    {
        return Err(TagError::NotAnElement(p.num));
    }
    let t = ty.trim();
    if t.is_empty() || t == "Artifact" || t.len() > 127 || !allowed_types(doc).iter().any(|a| a == t) {
        return Err(invalid(format!(
            "{t:?} is not a structure type: use a standard type (P, H1–H6, Figure, Table, L, LI, Span …) or a type in the role map"
        )));
    }
    let mut work = doc.clone();
    let mut d = Dict::new();
    d.set(b"Type".to_vec(), Object::name("StructElem"));
    d.set(b"S".to_vec(), Object::name(t));
    d.set(b"P".to_vec(), Object::Ref(parent.unwrap_or(root)));
    let r = work.add(Object::Dict(d));
    insert_kid(&mut work, root, parent, None, true, r)?;
    *doc = work;
    Ok(r)
}

/// Delete Tag: take `element` out of the structure tree. Its kids take its place in its parent:
/// child elements get the parent as their `/P`, marked content keeps its page (written as a
/// marked-content reference when it relied on the element's `/Pg`), and the parent tree points
/// the content and annotations at the parent. A top-level element that holds content directly
/// is refused (the tree root can hold only elements).
pub fn delete_element(doc: &mut Document, element: ObjRef) -> Result<DeleteResult, TagError> {
    let tree = tags(doc);
    if !tree.exists {
        return Err(TagError::Untagged);
    }
    if tree.truncated {
        return Err(invalid("the structure tree is too large or too deeply nested to edit safely"));
    }
    let root = root_ref(doc)?;
    let e = tree.find(element).ok_or(TagError::NotAnElement(element.num))?;
    let node = tree.nodes.get(e).ok_or(TagError::NotAnElement(element.num))?;
    let parent = match node.parent {
        None => None,
        Some(p) => Some(
            tree.nodes
                .get(p)
                .and_then(|n| n.obj)
                .ok_or_else(|| invalid(format!("element {}'s parent is written inside its own parent and can't be edited", element.num)))?,
        ),
    };
    let holds_content = node.kids.iter().any(|k| !matches!(k, TagKid::Element(_)));
    if parent.is_none() && holds_content {
        return Err(invalid(format!(
            "element {} is a top-level tag that holds content itself: its content would have no tag to go to (move the content into another tag first)",
            element.num
        )));
    }
    if node.kids.iter().any(|k| matches!(k, TagKid::Content { stream: Some(_), .. })) {
        return Err(invalid(format!("element {} holds content inside a form XObject, which can't be moved up yet", element.num)));
    }
    let holder = parent.unwrap_or(root);
    let pages = pdfcraft_model::pages(doc);
    let page_ref = node.page.and_then(|p| pages.get(p)).map(|p| p.obj);
    let d = doc.get(element).as_dict().cloned().ok_or_else(|| invalid("a structure element is not a dictionary"))?;
    let mut work = doc.clone();
    let mut lifted = Vec::new();
    for k in kid_list(&work, &d) {
        let kid = match k {
            // An MCID on the element's page: written with its page, as the parent may be on another.
            Object::Int(m) => match page_ref {
                Some(pg) => {
                    let mut mcr = Dict::new();
                    mcr.set(b"Type".to_vec(), Object::name("MCR"));
                    mcr.set(b"Pg".to_vec(), Object::Ref(pg));
                    mcr.set(b"MCID".to_vec(), Object::Int(m));
                    Object::Dict(mcr)
                }
                None => Object::Int(m),
            },
            Object::Dict(mut md) if matches!(md.name(b"Type"), Some(b"MCR") | Some(b"OBJR")) => {
                if !md.contains(b"Pg")
                    && let Some(pg) = page_ref
                {
                    md.set(b"Pg".to_vec(), Object::Ref(pg));
                }
                Object::Dict(md)
            }
            Object::Ref(r) => {
                let is_element = work.get(r).as_dict().is_some_and(|x| x.contains(b"S") && !matches!(x.name(b"Type"), Some(b"MCR") | Some(b"OBJR")));
                if is_element {
                    work.update_dict(r, |x| x.set(b"P".to_vec(), Object::Ref(holder)))?;
                }
                Object::Ref(r)
            }
            other => other,
        };
        lifted.push(kid);
    }
    let hd = work.get(holder).as_dict().cloned().ok_or_else(|| invalid("a structure element is not a dictionary"))?;
    let kids = kid_list(&work, &hd);
    let at = kids
        .iter()
        .position(|k| k.as_ref() == Some(element))
        .ok_or_else(|| invalid(format!("element {} is not listed in its parent's kids; it can't be deleted", element.num)))?;
    let count = lifted.len();
    let mut next: Vec<Object> = Vec::with_capacity(kids.len().saturating_add(count));
    next.extend(kids.iter().take(at).cloned());
    next.extend(lifted);
    next.extend(kids.iter().skip(at.saturating_add(1)).cloned());
    work.update_dict(holder, |x| x.set(b"K".to_vec(), Object::Array(next)))?;
    // The deleted element no longer points into the tree.
    work.update_dict(element, |x| {
        x.remove(b"P");
        x.remove(b"K");
    })?;
    let mut touched: Vec<usize> =
        node.kids.iter().filter_map(|k| if let TagKid::Content { page: Some(p), stream: None, .. } = k { Some(*p) } else { None }).collect();
    touched.sort_unstable();
    touched.dedup();
    for &p in &touched {
        let pg = pages.get(p).ok_or_else(|| invalid(format!("element {} names a page that doesn't exist", element.num)))?;
        rebuild_parent_tree(&mut work, root, p, pg.obj)?;
    }
    // Annotations it referenced: their parent-tree entry names the parent now.
    let annots: Vec<ObjRef> = node.kids.iter().filter_map(|k| if let TagKid::Object { obj, .. } = k { Some(*obj) } else { None }).collect();
    if !annots.is_empty() {
        let pt = parent_tree(&mut work, root)?;
        for a in annots {
            let key = work.get(a).as_dict().and_then(|x| x.get(b"StructParent").and_then(Object::as_int)).filter(|k| *k >= 0);
            if let Some(key) = key {
                put_number(&mut work, pt, key, Object::Ref(holder))?;
            }
        }
    }
    *doc = work;
    Ok(DeleteResult { lifted: count, parent, pages: touched })
}
