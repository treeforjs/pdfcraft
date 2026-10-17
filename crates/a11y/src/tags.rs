//! The Tags panel: the logical structure tree (ISO 32000-2 §14.7) read as a tree of elements
//! with their kids (elements, marked content, object references), and the element properties
//! the panel edits: the type, alternate text, actual text, language and title.
//!
//! Also the untagged half of Set Alternate Text: images painted on a page outside any tagged
//! (MCID) or artifact marked content, and tagging one as a Figure with alternate text.
//!
//! Every walk is bounded (element, kid and depth limits) and cycle-safe (each indirect object
//! is visited once), so a hostile tree reads as a truncated tree, never a hang or a crash.

use std::collections::{HashMap, HashSet};

use pdfcraft_content::Matrix;
use pdfcraft_cos::{Dict, Document, ObjRef, Object, PdfString, Stream};

use crate::structure::is_standard;

/// Elements beyond this are not read.
const MAX_ELEMENTS: usize = 500_000;
/// Kids of every kind (elements, MCIDs, OBJRs) beyond this are not read.
const MAX_KIDS: usize = 4_000_000;
/// Elements nested deeper than this are not read.
const MAX_DEPTH: usize = 512;
/// The longest alternate or actual text accepted (characters).
pub const MAX_TEXT: usize = 16_384;
/// Role map chains longer than this are treated as unmapped.
const MAX_ROLE_CHAIN: usize = 32;

/// One kid of a structure element, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum TagKid {
    /// A structure element: an index into [`Tags::nodes`].
    Element(usize),
    /// Marked content on a page (an MCID, directly or through a marked-content reference).
    Content {
        page: Option<usize>,
        mcid: i64,
        /// The content stream when it is not the page's (an MCR's /Stm, e.g. a form XObject).
        stream: Option<ObjRef>,
    },
    /// An object reference (OBJR): an annotation, a form field widget or an XObject.
    Object {
        obj: ObjRef,
        page: Option<usize>,
        /// The referenced object's /Subtype (Link, Widget, Image …).
        subtype: Option<String>,
    },
}

/// One structure element.
#[derive(Clone, Debug, PartialEq)]
pub struct TagNode {
    /// The element's object; `None` for an element written directly inside its parent (it
    /// can be shown but not edited).
    pub obj: Option<ObjRef>,
    pub parent: Option<usize>,
    /// 0 for top-level elements.
    pub depth: usize,
    /// /S as written.
    pub ty: String,
    /// The standard type /S maps to through the role map (`None`: it maps to none).
    pub standard: Option<String>,
    /// /T.
    pub title: Option<String>,
    /// /Alt.
    pub alt: Option<String>,
    /// /ActualText.
    pub actual_text: Option<String>,
    /// /Lang.
    pub lang: Option<String>,
    /// /ID.
    pub id: Option<String>,
    /// 0-based page from /Pg (its own or the nearest ancestor's).
    pub page: Option<usize>,
    pub kids: Vec<TagKid>,
}

impl TagNode {
    /// The element's own non-blank /Alt or /ActualText.
    pub fn has_alternate(&self) -> bool {
        let some = |s: &Option<String>| s.as_deref().is_some_and(|t| !t.trim().is_empty());
        some(&self.alt) || some(&self.actual_text)
    }

    /// The type to judge it by: the standard type, else the type as written.
    pub fn role(&self) -> &str {
        self.standard.as_deref().unwrap_or(&self.ty)
    }
}

/// The structure tree.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tags {
    /// The document has a /StructTreeRoot.
    pub exists: bool,
    /// The catalog's /MarkInfo /Marked is true.
    pub marked: bool,
    /// The catalog's /Lang.
    pub lang: Option<String>,
    /// Elements in document (pre-)order.
    pub nodes: Vec<TagNode>,
    /// Top-level elements, in order.
    pub roots: Vec<usize>,
    /// The role map: (custom type, the type it maps to), sorted.
    pub role_map: Vec<(String, String)>,
    /// A limit stopped the walk: some of the tree was not read.
    pub truncated: bool,
}

impl Tags {
    /// The node for an element object.
    pub fn find(&self, obj: ObjRef) -> Option<usize> {
        self.nodes.iter().position(|n| n.obj == Some(obj))
    }

    /// Ancestors of node `i`, nearest first (bounded by the depth limit).
    pub fn ancestors(&self, i: usize) -> impl Iterator<Item = usize> + '_ {
        std::iter::successors(self.nodes.get(i).and_then(|n| n.parent), move |p| self.nodes.get(*p).and_then(|n| n.parent)).take(MAX_DEPTH + 1)
    }

    /// The node or one of its ancestors has alternate or actual text (which replaces
    /// everything beneath it for assistive technology).
    pub fn covered_by_alternate(&self, i: usize) -> bool {
        self.nodes.get(i).is_some_and(TagNode::has_alternate) || self.ancestors(i).any(|a| self.nodes.get(a).is_some_and(TagNode::has_alternate))
    }
}

fn text(doc: &Document, d: &Dict, key: &[u8]) -> Option<String> {
    d.get(key).map(|o| doc.resolve(o)).and_then(|o| o.as_string().map(PdfString::to_text))
}

fn name_text(n: &[u8]) -> String {
    String::from_utf8_lossy(n).into_owned()
}

pub(crate) fn catalog(doc: &Document) -> Option<(ObjRef, Dict)> {
    let r = doc.root()?;
    let d = doc.get(r).as_dict().cloned()?;
    Some((r, d))
}

fn role_map(doc: &Document, root: &Dict) -> Dict {
    root.get(b"RoleMap").map(|o| doc.resolve(o)).and_then(|o| o.as_dict().cloned()).unwrap_or_default()
}

/// The standard type `ty` maps to through the role map, if any (cycles end the chain).
pub(crate) fn standard_type(doc: &Document, role_map: &Dict, ty: &[u8]) -> Option<Vec<u8>> {
    let mut t = ty.to_vec();
    let mut seen = HashSet::new();
    for _ in 0..MAX_ROLE_CHAIN {
        if is_standard(&t) {
            return Some(t);
        }
        if !seen.insert(t.clone()) {
            return None;
        }
        t = role_map.get(&t).map(|o| doc.resolve(o)).and_then(|o| o.as_name().map(<[u8]>::to_vec))?;
    }
    None
}

/// Read the structure tree.
pub fn tags(doc: &Document) -> Tags {
    let mut t = Tags::default();
    let Some((_, cat)) = catalog(doc) else { return t };
    t.lang = text(doc, &cat, b"Lang");
    t.marked = cat
        .get(b"MarkInfo")
        .map(|o| doc.resolve(o))
        .and_then(|o| o.as_dict().and_then(|d| d.get(b"Marked").map(|m| matches!(&*doc.resolve(m), Object::Bool(true)))))
        .unwrap_or(false);
    let Some(root) = cat.get(b"StructTreeRoot").map(|o| doc.resolve(o)).and_then(|o| o.as_dict().cloned()) else { return t };
    t.exists = true;
    let rm = role_map(doc, &root);
    let mut roles: Vec<(String, String)> = rm.iter().filter_map(|(k, v)| doc.resolve(v).as_name().map(|n| (name_text(k), name_text(n)))).collect();
    roles.sort();
    t.role_map = roles;
    let pages = pdfcraft_model::pages(doc);
    let page_index: HashMap<ObjRef, usize> = pages.iter().enumerate().map(|(i, p)| (p.obj, i)).collect();
    let page_of = |d: &Dict| d.get(b"Pg").and_then(Object::as_ref).and_then(|r| page_index.get(&r).copied());

    // Work items, popped in document order: (kid, parent node, inherited page, depth).
    let mut stack: Vec<(Object, Option<usize>, Option<usize>, usize)> = Vec::new();
    if let Some(k) = root.get(b"K") {
        stack.push((k.clone(), None, None, 0));
    }
    let mut seen: HashSet<ObjRef> = HashSet::new();
    let mut kids_read = 0usize;
    while let Some((k, parent, page, depth)) = stack.pop() {
        kids_read += 1;
        if kids_read > MAX_KIDS || t.nodes.len() >= MAX_ELEMENTS {
            t.truncated = true;
            break;
        }
        // References resolve once; a second visit is a cycle or a shared kid.
        let (k, obj) = match k {
            Object::Ref(r) => {
                if !seen.insert(r) {
                    continue;
                }
                ((*doc.get(r)).clone(), Some(r))
            }
            other => (other, None),
        };
        match k {
            Object::Array(a) => {
                stack.extend(a.into_iter().rev().map(|x| (x, parent, page, depth)));
            }
            Object::Int(mcid) => {
                if let Some(p) = parent.and_then(|p| t.nodes.get_mut(p)) {
                    p.kids.push(TagKid::Content { page, mcid, stream: None });
                }
            }
            Object::Dict(d) => match d.name(b"Type") {
                Some(b"MCR") => {
                    if let (Some(p), Some(mcid)) = (parent.and_then(|p| t.nodes.get_mut(p)), d.get(b"MCID").and_then(Object::as_int)) {
                        p.kids.push(TagKid::Content { page: page_of(&d).or(page), mcid, stream: d.get(b"Stm").and_then(Object::as_ref) });
                    }
                }
                Some(b"OBJR") => {
                    if let (Some(p), Some(o)) = (parent.and_then(|p| t.nodes.get_mut(p)), d.get(b"Obj").and_then(Object::as_ref)) {
                        let target = doc.get(o);
                        let subtype = match &*target {
                            Object::Dict(x) => x.name(b"Subtype").map(name_text),
                            Object::Stream(s) => s.dict.name(b"Subtype").map(name_text),
                            _ => None,
                        };
                        p.kids.push(TagKid::Object { obj: o, page: page_of(&d).or(page), subtype });
                    }
                }
                _ => {
                    let Some(s) = d.name(b"S") else { continue };
                    if depth > MAX_DEPTH {
                        t.truncated = true;
                        continue;
                    }
                    let i = t.nodes.len();
                    let page = page_of(&d).or(page);
                    t.nodes.push(TagNode {
                        obj,
                        parent,
                        depth,
                        ty: name_text(s),
                        standard: standard_type(doc, &rm, s).map(|v| name_text(&v)),
                        title: text(doc, &d, b"T"),
                        alt: text(doc, &d, b"Alt"),
                        actual_text: text(doc, &d, b"ActualText"),
                        lang: text(doc, &d, b"Lang"),
                        id: text(doc, &d, b"ID"),
                        page,
                        kids: Vec::new(),
                    });
                    match parent.and_then(|p| t.nodes.get_mut(p)) {
                        Some(p) => p.kids.push(TagKid::Element(i)),
                        None => t.roots.push(i),
                    }
                    if let Some(k) = d.get(b"K") {
                        stack.push((k.clone(), Some(i), page, depth + 1));
                    }
                }
            },
            _ => {}
        }
    }
    t
}

/// What to change on an element; `None` fields stay as they are. For the text fields,
/// `Some(None)` (or blank text) removes the entry.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TagChange {
    /// A new /S: a standard structure type or a type the role map maps to one.
    pub ty: Option<String>,
    pub alt: Option<Option<String>>,
    pub actual_text: Option<Option<String>>,
    pub lang: Option<Option<String>>,
    pub title: Option<Option<String>>,
}

impl TagChange {
    pub fn is_empty(&self) -> bool {
        self.ty.is_none() && self.alt.is_none() && self.actual_text.is_none() && self.lang.is_none() && self.title.is_none()
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum TagError {
    #[error("the document has no tags")]
    Untagged,
    #[error("object {0} is not an element of the structure tree")]
    NotAnElement(u32),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Cos(#[from] pdfcraft_cos::CosError),
}

/// A language tag (BCP 47 shape): letters, digits and hyphens, starting with a letter.
fn valid_lang(l: &str) -> bool {
    !l.is_empty()
        && l.len() <= 35
        && l.split('-').all(|p| !p.is_empty() && p.len() <= 8 && p.bytes().all(|b| b.is_ascii_alphanumeric()))
        && l.bytes().next().is_some_and(|b| b.is_ascii_alphabetic())
}

fn clean(v: &Option<String>, what: &str) -> Result<Option<String>, TagError> {
    match v.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) if s.chars().count() > MAX_TEXT => Err(TagError::Invalid(format!("{what} is longer than {MAX_TEXT} characters"))),
        Some(s) => Ok(Some(s.to_owned())),
        None => Ok(None),
    }
}

/// The structure types a tag may be changed to: the standard types and the role map's
/// custom types that map to one.
pub fn allowed_types(doc: &Document) -> Vec<String> {
    let mut out: Vec<String> = crate::structure::STANDARD.iter().filter(|t| **t != b"Artifact").map(|t| name_text(t)).collect();
    if let Some(root) = catalog(doc).and_then(|(_, c)| c.get(b"StructTreeRoot").map(|o| doc.resolve(o)).and_then(|o| o.as_dict().cloned())) {
        let rm = role_map(doc, &root);
        for (k, _) in rm.iter() {
            if !is_standard(k) && standard_type(doc, &rm, k).is_some() {
                out.push(name_text(k));
            }
        }
    }
    out
}

/// Change an element's properties, all or nothing.
pub fn set_tag(doc: &mut Document, element: ObjRef, change: &TagChange) -> Result<(), TagError> {
    if change.is_empty() {
        return Err(TagError::Invalid("nothing to change: give a type, alt, actual_text, lang or title".into()));
    }
    let tree = tags(doc);
    if !tree.exists {
        return Err(TagError::Untagged);
    }
    if tree.find(element).is_none() {
        return Err(TagError::NotAnElement(element.num));
    }
    let ty = match change.ty.as_deref().map(str::trim) {
        None => None,
        Some(t) => {
            let ok = !t.is_empty() && t != "Artifact" && allowed_types(doc).iter().any(|a| a == t);
            if !ok {
                return Err(TagError::Invalid(format!(
                    "{t:?} is not a structure type: use a standard type (P, H1–H6, Figure, Table, L, LI, Span …) or a type in the role map"
                )));
            }
            Some(t.to_owned())
        }
    };
    let alt = change.alt.as_ref().map(|v| clean(v, "alt")).transpose()?;
    let actual = change.actual_text.as_ref().map(|v| clean(v, "actual_text")).transpose()?;
    let title = change.title.as_ref().map(|v| clean(v, "title")).transpose()?;
    let lang = match &change.lang {
        None => None,
        Some(v) => {
            let l = clean(v, "lang")?;
            if let Some(l) = &l
                && !valid_lang(l)
            {
                return Err(TagError::Invalid(format!("{l:?} is not a language tag (for example en-US)")));
            }
            Some(l)
        }
    };
    doc.update_dict(element, |d| {
        if let Some(t) = &ty {
            d.set(b"S".to_vec(), Object::name(t));
        }
        let mut put = |key: &[u8], v: &Option<Option<String>>| match v {
            Some(Some(s)) => d.set(key.to_vec(), Object::String(PdfString::text(s))),
            Some(None) => {
                d.remove(key);
            }
            None => {}
        };
        put(b"Alt", &alt);
        put(b"ActualText", &actual);
        put(b"T", &title);
        put(b"Lang", &lang);
    })?;
    Ok(())
}

/// An image painted outside tagged and artifact content.
#[derive(Clone, Debug, PartialEq)]
pub struct UntaggedImage {
    /// 0-based page.
    pub page: usize,
    /// 0-based position among the page's untagged images, in painting order.
    pub index: usize,
    /// The XObject's resource name (`None` for an inline image).
    pub name: Option<String>,
    /// Where it is drawn, in user space.
    pub bbox: [f64; 4],
}

pub(crate) fn page_content(doc: &Document, page: &Dict) -> Vec<u8> {
    let Some(c) = page.get(b"Contents") else { return Vec::new() };
    match &*doc.resolve(c) {
        Object::Stream(s) => s.decoded().unwrap_or_default(),
        Object::Array(a) => a
            .iter()
            .flat_map(|x| match &*doc.resolve(x) {
                Object::Stream(s) => {
                    let mut v = s.decoded().unwrap_or_default();
                    v.push(b'\n');
                    v
                }
                _ => Vec::new(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

pub(crate) fn sub_dict(doc: &Document, d: &Dict, key: &[u8]) -> Dict {
    d.get(key).map(|o| doc.resolve(o)).and_then(|o| o.as_dict().cloned()).unwrap_or_default()
}

/// An untagged image found in a page's ops: its op index, name and box.
struct Found {
    op: usize,
    name: Option<String>,
    bbox: [f64; 4],
}

/// The page's top-level ops, the untagged images among them, and the largest MCID used.
fn scan_page(doc: &Document, page: &Dict) -> (Vec<pdfcraft_content::Op>, Vec<Found>, Option<i64>) {
    let ops = pdfcraft_content::parse(&page_content(doc, page)).ops;
    let res = sub_dict(doc, page, b"Resources");
    let xobjects = sub_dict(doc, &res, b"XObject");
    let props = sub_dict(doc, &res, b"Properties");
    let identity = Matrix([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    let mut ctm = vec![identity];
    // Each open marked-content sequence: does it make its content tagged or an artifact?
    let mut marks: Vec<bool> = Vec::new();
    let mut found = Vec::new();
    let mut max_mcid: Option<i64> = None;
    for (i, op) in ops.iter().enumerate() {
        let top = ctm.last().copied().unwrap_or(identity);
        match op.op.as_slice() {
            b"q" if ctm.len() < 4096 => ctm.push(top),
            b"Q" if ctm.len() > 1 => {
                ctm.pop();
            }
            b"cm" => {
                if let Some(m) = Matrix::from_operands(&op.operands)
                    && let Some(t) = ctm.last_mut()
                {
                    *t = m.then(&top);
                }
            }
            b"BMC" if marks.len() < 4096 => marks.push(op.name(0) == Some(b"Artifact")),
            b"BDC" if marks.len() < 4096 => {
                let p = match op.operands.get(1) {
                    Some(Object::Dict(d)) => Some(d.clone()),
                    Some(Object::Name(n)) => props.get(n).map(|x| doc.resolve(x)).and_then(|x| x.as_dict().cloned()),
                    _ => None,
                };
                let mcid = p.as_ref().and_then(|d| d.get(b"MCID").and_then(Object::as_int));
                if let Some(m) = mcid {
                    max_mcid = Some(max_mcid.map_or(m, |x| x.max(m)));
                }
                marks.push(mcid.is_some() || op.name(0) == Some(b"Artifact"));
            }
            b"EMC" => {
                marks.pop();
            }
            b"Do" if !marks.iter().any(|m| *m) => {
                let Some(n) = op.name(0) else { continue };
                let is_image = xobjects.get(n).map(|x| doc.resolve(x)).is_some_and(|x| match &*x {
                    Object::Stream(s) => s.dict.name(b"Subtype") == Some(b"Image"),
                    _ => false,
                });
                if is_image {
                    found.push(Found { op: i, name: Some(name_text(n)), bbox: top.bbox([0.0, 0.0, 1.0, 1.0]) });
                }
            }
            b"BI" if !marks.iter().any(|m| *m) => found.push(Found { op: i, name: None, bbox: top.bbox([0.0, 0.0, 1.0, 1.0]) }),
            _ => {}
        }
    }
    (ops, found, max_mcid)
}

/// Images painted directly on the pages (not inside form XObjects) outside tagged and
/// artifact content, page by page.
pub fn untagged_images(doc: &Document) -> Vec<UntaggedImage> {
    let mut out = Vec::new();
    for (p, page) in pdfcraft_model::pages(doc).iter().enumerate() {
        let (_, found, _) = scan_page(doc, &page.dict);
        out.extend(found.into_iter().enumerate().map(|(index, f)| UntaggedImage { page: p, index, name: f.name, bbox: f.bbox }));
    }
    out
}

/// The entries of a number tree that hold `key`: the node and its /Nums, bounded.
pub(crate) fn number_tree_leaf(doc: &Document, root: ObjRef, key: i64) -> Option<(ObjRef, Vec<Object>)> {
    let mut stack = vec![root];
    let mut seen = HashSet::new();
    while let Some(r) = stack.pop() {
        if !seen.insert(r) || seen.len() > 100_000 {
            continue;
        }
        let Some(d) = doc.get(r).as_dict().cloned() else { continue };
        if let Some(nums) = d.get(b"Nums").map(|o| doc.resolve(o)).and_then(|o| o.as_array().cloned())
            && nums.chunks(2).any(|c| c.first().and_then(Object::as_int) == Some(key))
        {
            return Some((r, nums));
        }
        if let Some(kids) = d.get(b"Kids").map(|o| doc.resolve(o)).and_then(|o| o.as_array().cloned()) {
            stack.extend(kids.iter().filter_map(Object::as_ref));
        }
    }
    None
}

/// The largest key in a number tree, bounded.
fn number_tree_max(doc: &Document, root: ObjRef) -> Option<i64> {
    let mut stack = vec![root];
    let mut seen = HashSet::new();
    let mut max = None;
    while let Some(r) = stack.pop() {
        if !seen.insert(r) || seen.len() > 100_000 {
            continue;
        }
        let Some(d) = doc.get(r).as_dict().cloned() else { continue };
        if let Some(nums) = d.get(b"Nums").map(|o| doc.resolve(o)).and_then(|o| o.as_array().cloned()) {
            for k in nums.chunks(2).filter_map(|c| c.first().and_then(Object::as_int)) {
                max = Some(max.map_or(k, |m: i64| m.max(k)));
            }
        }
        if let Some(kids) = d.get(b"Kids").map(|o| doc.resolve(o)).and_then(|o| o.as_array().cloned()) {
            stack.extend(kids.iter().filter_map(Object::as_ref));
        }
    }
    max
}

/// The largest MCID a parent-tree array may index is bounded so a hostile MCID can't make a
/// huge array.
pub(crate) const MAX_MCID: i64 = 1_000_000;

/// Set Alternate Text on an untagged image: wrap it in Figure marked content, add a Figure
/// element with the alternate text to the structure tree (creating a minimal tree for an
/// untagged document) and point the page's parent-tree entry at it. Returns the new element.
///
/// The figure goes at the end of the top-level element (Document) or of the root; reading
/// order can be adjusted afterwards.
pub fn tag_image(doc: &mut Document, page: usize, index: usize, alt: &str) -> Result<ObjRef, TagError> {
    let alt = clean(&Some(alt.to_owned()), "alt")?.ok_or_else(|| TagError::Invalid("give the alternate text".into()))?;
    let pages = pdfcraft_model::pages(doc);
    let pg = pages.get(page).ok_or_else(|| TagError::Invalid(format!("there is no page {}", page.saturating_add(1))))?;
    let (mut ops, found, max_mcid) = scan_page(doc, &pg.dict);
    let target = found.get(index).ok_or_else(|| {
        TagError::Invalid(format!("page {} has {} untagged image(s); there is no image {}", page + 1, found.len(), index.saturating_add(1)))
    })?;
    let op_at = target.op;
    let (cat_ref, cat) = catalog(doc).ok_or(TagError::Untagged)?;

    // Validate the existing tree before changing anything.
    let existing_root = match cat.get(b"StructTreeRoot") {
        None => None,
        Some(Object::Ref(r)) if doc.get(*r).as_dict().is_some() => Some(*r),
        Some(_) => return Err(TagError::Invalid("the structure tree root is not an indirect dictionary; it can't be extended".into())),
    };
    let parent_tree = existing_root.and_then(|r| doc.get(r).as_dict().and_then(|d| d.get(b"ParentTree").cloned()));
    let parent_tree = match parent_tree {
        None => None,
        Some(Object::Ref(r)) if doc.get(r).as_dict().is_some() => Some(r),
        Some(_) => return Err(TagError::Invalid("the parent tree is not an indirect dictionary; it can't be extended".into())),
    };
    let page_key = pg.dict.get(b"StructParents").and_then(Object::as_int);
    let leaf = match (parent_tree, page_key) {
        (Some(pt), Some(k)) => number_tree_leaf(doc, pt, k),
        _ => None,
    };
    let existing_len = leaf
        .as_ref()
        .and_then(|(_, nums)| nums.chunks(2).find(|c| c.first().and_then(Object::as_int) == page_key).and_then(|c| c.get(1)).map(|v| doc.resolve(v)))
        .and_then(|v| v.as_array().map(Vec::len))
        .unwrap_or(0);
    let mcid = max_mcid.map_or(0, |m| m.saturating_add(1)).max(i64::try_from(existing_len).unwrap_or(MAX_MCID));
    if !(0..MAX_MCID).contains(&mcid) {
        return Err(TagError::Invalid("the page's marked-content ids are out of range".into()));
    }

    // The tree root, its container and parent tree (created for an untagged document).
    let root_ref = match existing_root {
        Some(r) => r,
        None => {
            let r = doc.add(Object::Dict(Dict::new()));
            let pt = doc.add(Object::Dict({
                let mut d = Dict::new();
                d.set(b"Nums".to_vec(), Object::Array(Vec::new()));
                d
            }));
            let top = doc.add(Object::Dict({
                let mut d = Dict::new();
                d.set(b"Type".to_vec(), Object::name("StructElem"));
                d.set(b"S".to_vec(), Object::name("Document"));
                d.set(b"P".to_vec(), Object::Ref(r));
                d.set(b"K".to_vec(), Object::Array(Vec::new()));
                d
            }));
            doc.set(r, {
                let mut d = Dict::new();
                d.set(b"Type".to_vec(), Object::name("StructTreeRoot"));
                d.set(b"K".to_vec(), Object::Array(vec![Object::Ref(top)]));
                d.set(b"ParentTree".to_vec(), Object::Ref(pt));
                d.set(b"ParentTreeNextKey".to_vec(), Object::Int(0));
                Object::Dict(d)
            });
            doc.update_dict(cat_ref, |c| c.set(b"StructTreeRoot".to_vec(), Object::Ref(r)))?;
            r
        }
    };
    let root = doc.get(root_ref).as_dict().cloned().unwrap_or_default();
    let parent_tree = match parent_tree {
        Some(p) => p,
        None => match root.get(b"ParentTree") {
            Some(Object::Ref(r)) => *r,
            _ => {
                let pt = doc.add(Object::Dict({
                    let mut d = Dict::new();
                    d.set(b"Nums".to_vec(), Object::Array(Vec::new()));
                    d
                }));
                doc.update_dict(root_ref, |d| d.set(b"ParentTree".to_vec(), Object::Ref(pt)))?;
                pt
            }
        },
    };
    // The container: the single top-level element, else the root itself.
    let top: Vec<ObjRef> = match root.get(b"K").map(|o| (o.clone(), doc.resolve(o))) {
        Some((Object::Ref(r), _)) if doc.get(r).as_dict().is_some_and(|d| d.contains(b"S")) => vec![r],
        Some((_, a)) => a.as_array().map(|a| a.iter().filter_map(Object::as_ref).collect()).unwrap_or_default(),
        None => Vec::new(),
    };
    let container = match top.as_slice() {
        [one] if doc.get(*one).as_dict().is_some_and(|d| d.contains(b"S")) => *one,
        _ => root_ref,
    };

    // The figure element.
    let figure = doc.add(Object::Dict({
        let mut d = Dict::new();
        d.set(b"Type".to_vec(), Object::name("StructElem"));
        d.set(b"S".to_vec(), Object::name("Figure"));
        d.set(b"P".to_vec(), Object::Ref(container));
        d.set(b"Pg".to_vec(), Object::Ref(pg.obj));
        d.set(b"K".to_vec(), Object::Int(mcid));
        d.set(b"Alt".to_vec(), Object::String(PdfString::text(&alt)));
        d
    }));
    let kids = doc.get(container).as_dict().and_then(|d| d.get(b"K").cloned());
    let mut list = match kids.as_ref().map(|k| (k.clone(), doc.resolve(k))) {
        None => Vec::new(),
        Some((Object::Array(a), _)) => a,
        Some((Object::Ref(r), resolved)) => match resolved.as_array() {
            Some(a) if doc.get(r).as_dict().is_none() => a.clone(),
            _ => vec![Object::Ref(r)],
        },
        Some((other, _)) => vec![other],
    };
    list.push(Object::Ref(figure));
    doc.update_dict(container, |d| d.set(b"K".to_vec(), Object::Array(list)))?;

    // The page content: the image op inside Figure marked content.
    let mut props = Dict::new();
    props.set(b"MCID".to_vec(), Object::Int(mcid));
    let wrapped = vec![pdfcraft_content::Op::new("BDC", vec![Object::name("Figure"), Object::Dict(props)])];
    let tail = ops.split_off(op_at);
    let mut tail = tail.into_iter();
    ops.extend(wrapped);
    ops.extend(tail.next());
    ops.push(pdfcraft_content::Op::new("EMC", Vec::new()));
    ops.extend(tail);
    let s = doc.add(Object::Stream(Stream::flate(Dict::new(), &pdfcraft_content::serialize_ops(&ops))));

    // The parent tree: the page's array gets the figure at its MCID.
    let mcid_at = usize::try_from(mcid).unwrap_or(0);
    let place = |a: &mut Vec<Object>| {
        if a.len() <= mcid_at {
            a.resize(mcid_at + 1, Object::Null);
        }
        if let Some(slot) = a.get_mut(mcid_at) {
            *slot = Object::Ref(figure);
        }
    };
    let key = match (page_key, leaf) {
        (Some(k), Some((node, mut nums))) => {
            for pair in nums.chunks_mut(2) {
                if pair.first().and_then(Object::as_int) != Some(k) {
                    continue;
                }
                match pair.get(1).cloned() {
                    Some(Object::Ref(r)) => {
                        let mut a = doc.get(r).as_array().cloned().unwrap_or_default();
                        place(&mut a);
                        doc.set(r, Object::Array(a));
                    }
                    Some(Object::Array(mut a)) => {
                        place(&mut a);
                        if let Some(v) = pair.get_mut(1) {
                            *v = Object::Array(a);
                        }
                    }
                    _ => {
                        let mut a = Vec::new();
                        place(&mut a);
                        if let Some(v) = pair.get_mut(1) {
                            *v = Object::Array(a);
                        }
                    }
                }
            }
            doc.update_dict(node, |d| d.set(b"Nums".to_vec(), Object::Array(nums)))?;
            k
        }
        _ => {
            // A new key past every key in use (and past an existing key that has no entry).
            let next = root.get(b"ParentTreeNextKey").and_then(Object::as_int).unwrap_or(0);
            let k = number_tree_max(doc, parent_tree).map_or(0, |m| m.saturating_add(1)).max(next);
            let mut a = Vec::new();
            place(&mut a);
            let arr = doc.add(Object::Array(a));
            let pt = doc.get(parent_tree).as_dict().cloned().unwrap_or_default();
            if pt.contains(b"Kids") {
                // A new leaf at the end: keys only grow.
                let leaf = doc.add(Object::Dict({
                    let mut d = Dict::new();
                    d.set(b"Limits".to_vec(), Object::Array(vec![Object::Int(k), Object::Int(k)]));
                    d.set(b"Nums".to_vec(), Object::Array(vec![Object::Int(k), Object::Ref(arr)]));
                    d
                }));
                doc.update_dict(parent_tree, |d| {
                    if let Some(Object::Array(kids)) = d.get_mut(b"Kids") {
                        kids.push(Object::Ref(leaf));
                    }
                    if let Some(Object::Array(l)) = d.get_mut(b"Limits")
                        && let Some(hi) = l.get_mut(1)
                    {
                        *hi = Object::Int(k);
                    }
                })?;
            } else {
                let mut nums = pt.get(b"Nums").map(|o| doc.resolve(o)).and_then(|o| o.as_array().cloned()).unwrap_or_default();
                nums.push(Object::Int(k));
                nums.push(Object::Ref(arr));
                doc.update_dict(parent_tree, |d| d.set(b"Nums".to_vec(), Object::Array(nums)))?;
            }
            doc.update_dict(root_ref, |d| d.set(b"ParentTreeNextKey".to_vec(), Object::Int(k.saturating_add(1))))?;
            k
        }
    };
    let pg_obj = pg.obj;
    doc.update_dict(pg_obj, |p| {
        p.set(b"Contents".to_vec(), Object::Ref(s));
        p.set(b"StructParents".to_vec(), Object::Int(key));
    })?;
    Ok(figure)
}

/// A tagged figure in the Set Alternate Text walk.
#[derive(Clone, Debug, PartialEq)]
pub struct FigureTarget {
    /// The node in [`Tags::nodes`].
    pub node: usize,
    /// `None` for a direct element, which can't be edited.
    pub obj: Option<ObjRef>,
    pub page: Option<usize>,
    pub alt: Option<String>,
    pub actual_text: Option<String>,
    /// Neither it nor an ancestor has alternate or actual text: the checker's
    /// figures-alt-text rule fails on it.
    pub needs_alt: bool,
    /// Where its page content is drawn, in user space.
    pub bbox: Option<[f64; 4]>,
}

/// Everything Set Alternate Text walks through: the Figure elements (through the role map)
/// and the images painted outside tagged and artifact content.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AltTargets {
    pub figures: Vec<FigureTarget>,
    pub images: Vec<UntaggedImage>,
}

pub fn alt_targets(doc: &Document) -> AltTargets {
    let tree = tags(doc);
    let pages = pdfcraft_model::pages(doc);
    let figures = tree
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.role() == "Figure")
        .map(|(i, n)| {
            let ids: HashSet<i64> = n
                .kids
                .iter()
                .filter_map(|k| match k {
                    TagKid::Content { mcid, stream: None, page } if *page == n.page => Some(*mcid),
                    _ => None,
                })
                .collect();
            let bbox = n.page.and_then(|p| pages.get(p)).filter(|_| !ids.is_empty()).and_then(|p| crate::alt::content_bbox(doc, &p.dict, &ids));
            FigureTarget {
                node: i,
                obj: n.obj,
                page: n.page,
                alt: n.alt.clone(),
                actual_text: n.actual_text.clone(),
                needs_alt: !tree.covered_by_alternate(i),
                bbox,
            }
        })
        .collect();
    AltTargets { figures, images: untagged_images(doc) }
}
