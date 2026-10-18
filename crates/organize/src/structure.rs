//! Merging logical structure trees (tags, ISO 32000-2 §14.7) when pages are copied between
//! documents: Combine Files, Insert Pages from File, Extract Pages and Split.
//!
//! Pages are copied first (`import`), without their `/StructParents` and their annotations'
//! `/StructParent`. Then the part of each source's structure tree whose content is on the copied
//! pages is copied too, and linked to the copies:
//! - the elements go under the destination's one `Document` element (created, or wrapped
//!   around the existing top-level elements), at the position of their pages. A source's own
//!   top-level `Document` element is dissolved into it, its language moving onto its kids;
//! - marked-content IDs stay as they are (they number marked content per content stream, and
//!   every copied page keeps its own streams); the parent tree is rebuilt with new keys
//!   (`/StructParents`, `/StructParent`, `/ParentTreeNextKey`) for the copied pages, form
//!   XObjects and annotations;
//! - role maps are merged: a type the destination already maps differently keeps the first
//!   mapping, and the conflict is reported;
//! - class maps are merged (a different class under a used name is renamed, and the elements
//!   that use it follow), element IDs are made unique in the merged `/IDTree`, and PDF 2.0
//!   namespaces are merged (a namespace with the same URI and role mapping is shared);
//! - the result is marked tagged (`/MarkInfo /Marked true`) only when every part of it came
//!   from a tagged document; otherwise the report says it is partially tagged.
//!
//! Elements are kept when some of their content (marked content, or an annotation or XObject
//! they reference) is on a copied page; elements with no content at all are kept when their
//! page is copied (or, without a page, when the whole source is).
//!
//! Every walk is bounded (element, kid, depth and entry limits) and cycle-safe, so a hostile tree
//! is merged truncated, with a warning, never a hang or a crash.

use std::collections::{BTreeMap, HashMap, HashSet};

use pdfcraft_cos::{Dict, Document, ObjRef, Object, PdfString};

use crate::OrganizeError;
use crate::import::{Copier, Imported};

/// Elements beyond this are not read.
const MAX_ELEMENTS: usize = 500_000;
/// Kids of every kind beyond this are not read.
const MAX_KIDS: usize = 4_000_000;
/// Elements nested deeper than this are not read.
const MAX_DEPTH: usize = 512;
/// Number and name tree limits.
const MAX_TREE_DEPTH: usize = 32;
const MAX_TREE_ENTRIES: usize = 1_000_000;
/// Parent-tree array slots (one per marked-content ID up to the largest on a page) written per
/// merge: a hostile MCID would otherwise size an array.
const MAX_PARENT_SLOTS: usize = 2_000_000;
/// Marked-content IDs above this are not merged (each one sizes a parent-tree array).
const MAX_MCID: i64 = 1_000_000;
/// Role map chains longer than this are treated as unmapped.
const MAX_ROLE_CHAIN: usize = 32;
/// Comparing two values (class attributes, namespaces) gives up beyond this.
const MAX_COMPARE_DEPTH: usize = 8;
const MAX_COMPARE_NODES: usize = 10_000;
/// Warnings kept per report.
const MAX_WARNINGS: usize = 200;

/// A type mapped differently by two of the merged role maps.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoleMapConflict {
    /// The custom type.
    pub role: String,
    /// What it maps to in the result (the first mapping).
    pub kept: String,
    /// The mapping that lost.
    pub ignored: String,
    /// The source whose mapping lost.
    pub source: String,
}

/// What merging structure trees did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TagMergeReport {
    /// Parts of the result that came from a tagged document (a structure tree and
    /// `/MarkInfo /Marked true`): the sources and, for an insert, the destination.
    pub tagged_sources: usize,
    /// Parts that came from a document that is not tagged.
    pub untagged_sources: usize,
    /// Structure elements copied into the result.
    pub elements: usize,
    /// Elements left out because none of their content is on the copied pages.
    pub elements_dropped: usize,
    /// The result has a structure tree.
    pub tree: bool,
    /// The result's `/MarkInfo /Marked`.
    pub marked: bool,
    /// The result has a structure tree but some of its pages came from an untagged document.
    pub partially_tagged: bool,
    pub role_map_conflicts: Vec<RoleMapConflict>,
    /// Element IDs changed to keep them unique: (old, new).
    pub ids_renamed: Vec<(String, String)>,
    /// Classes added under a new name because the name was taken: (old, new).
    pub classes_renamed: Vec<(String, String)>,
    /// PDF 2.0 namespaces added to the result's `/Namespaces`.
    pub namespaces_added: usize,
    /// A limit stopped a walk: part of a source tree was not merged.
    pub truncated: bool,
    /// Everything worth telling the user, in order.
    pub warnings: Vec<String>,
}

impl TagMergeReport {
    fn warn(&mut self, w: String) {
        if self.warnings.len() < MAX_WARNINGS {
            self.warnings.push(w);
        }
    }
}

fn name_text(n: &[u8]) -> String {
    String::from_utf8_lossy(n).into_owned()
}

fn catalog(doc: &Document) -> Option<(ObjRef, Dict)> {
    let r = doc.root()?;
    let d = doc.get(r).as_dict().cloned()?;
    Some((r, d))
}

fn resolved_dict(doc: &Document, o: Option<&Object>) -> Option<Dict> {
    o.map(|o| doc.resolve(o)).and_then(|o| o.as_dict().cloned())
}

fn tree_root(doc: &Document) -> Option<Dict> {
    let (_, cat) = catalog(doc)?;
    resolved_dict(doc, cat.get(b"StructTreeRoot"))
}

fn mark_info(doc: &Document) -> Dict {
    catalog(doc).and_then(|(_, c)| resolved_dict(doc, c.get(b"MarkInfo"))).unwrap_or_default()
}

fn flag(doc: &Document, d: &Dict, key: &[u8]) -> bool {
    d.get(key).is_some_and(|m| matches!(&*doc.resolve(m), Object::Bool(true)))
}

/// The document has a structure tree and says it is tagged (`/MarkInfo /Marked true`).
pub fn is_tagged(doc: &Document) -> bool {
    tree_root(doc).is_some() && flag(doc, &mark_info(doc), b"Marked")
}

/// The catalog's `/Lang`.
fn doc_lang(doc: &Document) -> Option<PdfString> {
    let (_, cat) = catalog(doc)?;
    cat.get(b"Lang").map(|l| doc.resolve(l)).and_then(|l| l.as_string().cloned())
}

/// `ty` is `target`, or maps to it through the role map.
fn maps_to(doc: &Document, role_map: &Dict, ty: &[u8], target: &[u8]) -> bool {
    let mut t = ty.to_vec();
    for _ in 0..MAX_ROLE_CHAIN {
        if t == target {
            return true;
        }
        match role_map.get(&t).map(|o| doc.resolve(o)).and_then(|o| o.as_name().map(<[u8]>::to_vec)) {
            Some(n) if n != t => t = n,
            _ => return false,
        }
    }
    false
}

/// The state of a destination before pages are copied into it.
pub(crate) struct Before {
    had_pages: bool,
    tagged: bool,
    tree: bool,
    suspects: bool,
}

impl Before {
    pub(crate) fn of(dst: &Document) -> Result<Before, OrganizeError> {
        let had_pages = crate::page_count(dst).map(|n| n > 0).unwrap_or(false);
        Ok(Before { had_pages, tagged: is_tagged(dst), tree: tree_root(dst).is_some(), suspects: flag(dst, &mark_info(dst), b"Suspects") })
    }
}

/// Which parts of a result were tagged, for its `/MarkInfo`.
#[derive(Default)]
pub(crate) struct Tally {
    tagged: usize,
    untagged: usize,
    untagged_names: Vec<String>,
    tree: bool,
    suspects: bool,
}

impl Tally {
    /// Count a source of pages.
    pub(crate) fn add(&mut self, src: &Document, name: &str) {
        self.add_part(is_tagged(src), tree_root(src).is_some(), flag(src, &mark_info(src), b"Suspects"), Some(name));
    }

    fn add_part(&mut self, tagged: bool, tree: bool, suspects: bool, name: Option<&str>) {
        if tagged {
            self.tagged += 1;
        } else {
            self.untagged += 1;
            if let Some(n) = name
                && self.untagged_names.len() < 20
            {
                self.untagged_names.push(n.to_string());
            }
        }
        self.tree |= tree;
        self.suspects |= suspects;
    }

    /// Mark the result tagged when every part was, and record it in the report. Nothing happens
    /// when no part had a structure tree.
    pub(crate) fn finish(&self, dst: &mut Document, report: &mut TagMergeReport) -> Result<(), OrganizeError> {
        report.tagged_sources = self.tagged;
        report.untagged_sources = self.untagged;
        // Nothing was tagged, or nothing of the tags survived (a tree without elements).
        if tree_root(dst).is_none() {
            return Ok(());
        }
        let marked = self.untagged == 0 && self.tagged > 0;
        set_mark_info(dst, marked, self.suspects)?;
        report.tree = tree_root(dst).is_some();
        report.marked = marked;
        report.partially_tagged = report.tree && !marked;
        if report.partially_tagged {
            let which = if self.untagged_names.is_empty() { String::new() } else { format!(" ({})", self.untagged_names.join(", ")) };
            report.warn(format!(
                "the result is partially tagged: {} of {} parts{which} came from untagged documents, so it is not marked as tagged",
                self.untagged,
                self.untagged + self.tagged
            ));
        }
        Ok(())
    }
}

/// Set the catalog's `/MarkInfo /Marked` (and `/Suspects` when any part had it).
fn set_mark_info(dst: &mut Document, marked: bool, suspects: bool) -> Result<(), OrganizeError> {
    let (root, cat) = catalog(dst).ok_or(OrganizeError::NoPageTree)?;
    let apply = |d: &mut Dict| {
        d.set(b"Marked".to_vec(), Object::Bool(marked));
        if suspects {
            d.set(b"Suspects".to_vec(), Object::Bool(true));
        }
    };
    match cat.get(b"MarkInfo") {
        Some(Object::Ref(r)) if dst.get(*r).as_dict().is_some() => dst.update_dict(*r, apply)?,
        _ => {
            let mut d = resolved_dict(dst, cat.get(b"MarkInfo")).unwrap_or_default();
            apply(&mut d);
            dst.update_dict(root, |c| c.set(b"MarkInfo".to_vec(), Object::Dict(d)))?;
        }
    }
    Ok(())
}

/// Merge `src`'s structure tree into `dst` after `imported` was copied into it (Insert Pages,
/// Extract Pages), and set `/MarkInfo` from both sides.
pub(crate) fn merge_import(
    dst: &mut Document,
    src: &Document,
    imported: &Imported,
    before: &Before,
    report: &mut TagMergeReport,
) -> Result<(), OrganizeError> {
    let mut tally = Tally::default();
    if before.had_pages {
        tally.add_part(before.tagged, before.tree, before.suspects, Some("the document"));
    }
    tally.add_part(is_tagged(src), tree_root(src).is_some(), flag(src, &mark_info(src), b"Suspects"), Some("the inserted pages"));
    if !tally.tree {
        report.tagged_sources = tally.tagged;
        report.untagged_sources = tally.untagged;
        return Ok(());
    }
    merge_source(dst, src, "the inserted pages", imported, before, report)?;
    tally.finish(dst, report)
}

/// One kid of a source element.
enum Kid {
    Element(usize),
    /// Marked content: `page` is the source page it is on (the MCR's /Pg, else the element's,
    /// else an ancestor's), `stm` the content stream when it is not the page's.
    Content {
        page: Option<ObjRef>,
        mcid: i64,
        stm: Option<ObjRef>,
        stm_own: Option<Object>,
    },
    /// An object reference (annotation, XObject).
    Object {
        page: Option<ObjRef>,
        obj: ObjRef,
    },
}

struct Elem {
    /// `None` for an element written directly in its parent.
    obj: Option<ObjRef>,
    dict: Dict,
    parent: Option<usize>,
    kids: Vec<Kid>,
}

struct SrcTree {
    root: Dict,
    elems: Vec<Elem>,
    roots: Vec<usize>,
    truncated: bool,
}

/// Read a source structure tree, bounded and cycle-safe.
fn read_tree(src: &Document, root: Dict) -> SrcTree {
    let mut t = SrcTree { root, elems: Vec::new(), roots: Vec::new(), truncated: false };
    let mut stack: Vec<(Object, Option<usize>, Option<ObjRef>, usize)> = Vec::new();
    if let Some(k) = t.root.get(b"K") {
        stack.push((k.clone(), None, None, 0));
    }
    let mut seen: HashSet<ObjRef> = HashSet::new();
    let mut kids_read = 0usize;
    while let Some((k, parent, page, depth)) = stack.pop() {
        kids_read += 1;
        if kids_read > MAX_KIDS || t.elems.len() >= MAX_ELEMENTS {
            t.truncated = true;
            break;
        }
        let (k, obj) = match k {
            Object::Ref(r) => {
                if !seen.insert(r) {
                    continue;
                }
                ((*src.get(r)).clone(), Some(r))
            }
            other => (other, None),
        };
        let pg = |d: &Dict| d.get(b"Pg").and_then(Object::as_ref);
        match k {
            Object::Array(a) => {
                // An array is never an element itself: a nested array is malformed, but read.
                stack.extend(a.into_iter().rev().map(|x| (x, parent, page, depth)));
            }
            Object::Int(mcid) => {
                if let Some(p) = parent.and_then(|p| t.elems.get_mut(p)) {
                    p.kids.push(Kid::Content { page, mcid, stm: None, stm_own: None });
                }
            }
            Object::Dict(d) => match d.name(b"Type") {
                Some(b"MCR") => {
                    if let (Some(p), Some(mcid)) = (parent.and_then(|p| t.elems.get_mut(p)), d.get(b"MCID").and_then(Object::as_int)) {
                        p.kids.push(Kid::Content {
                            page: pg(&d).or(page),
                            mcid,
                            stm: d.get(b"Stm").and_then(Object::as_ref),
                            stm_own: d.get(b"StmOwn").cloned(),
                        });
                    }
                }
                Some(b"OBJR") => {
                    if let (Some(p), Some(o)) = (parent.and_then(|p| t.elems.get_mut(p)), d.get(b"Obj").and_then(Object::as_ref)) {
                        p.kids.push(Kid::Object { page: pg(&d).or(page), obj: o });
                    }
                }
                _ => {
                    if d.name(b"S").is_none() {
                        continue;
                    }
                    if depth > MAX_DEPTH {
                        t.truncated = true;
                        continue;
                    }
                    let i = t.elems.len();
                    let page = pg(&d).or(page);
                    let kids = d.get(b"K").cloned();
                    t.elems.push(Elem { obj, dict: d, parent, kids: Vec::new() });
                    match parent.and_then(|p| t.elems.get_mut(p)) {
                        Some(p) => p.kids.push(Kid::Element(i)),
                        None => t.roots.push(i),
                    }
                    if let Some(k) = kids {
                        stack.push((k, Some(i), page, depth + 1));
                    }
                }
            },
            _ => {}
        }
    }
    t
}

/// Every (key, value) of a number tree, bounded and cycle-safe.
fn number_tree(doc: &Document, node: &Object, depth: usize, seen: &mut HashSet<ObjRef>, out: &mut Vec<(i64, Object)>) {
    if depth > MAX_TREE_DEPTH || out.len() >= MAX_TREE_ENTRIES {
        return;
    }
    if let Object::Ref(r) = node
        && !seen.insert(*r)
    {
        return;
    }
    let Some(d) = doc.resolve(node).as_dict().cloned() else { return };
    if let Some(nums) = d.get(b"Nums").map(|n| doc.resolve(n)).and_then(|n| n.as_array().cloned()) {
        for pair in nums.chunks(2) {
            if let [k, v] = pair
                && let Some(k) = doc.resolve(k).as_int()
                && out.len() < MAX_TREE_ENTRIES
            {
                out.push((k, v.clone()));
            }
        }
    }
    for k in d.get(b"Kids").map(|k| doc.resolve(k)).and_then(|k| k.as_array().cloned()).unwrap_or_default() {
        number_tree(doc, &k, depth + 1, seen, out);
    }
}

/// Every (key, value) of a name tree, bounded and cycle-safe.
fn name_tree(doc: &Document, node: &Object, depth: usize, seen: &mut HashSet<ObjRef>, out: &mut Vec<(Vec<u8>, Object)>) {
    if depth > MAX_TREE_DEPTH || out.len() >= MAX_TREE_ENTRIES {
        return;
    }
    if let Object::Ref(r) = node
        && !seen.insert(*r)
    {
        return;
    }
    let Some(d) = doc.resolve(node).as_dict().cloned() else { return };
    if let Some(names) = d.get(b"Names").map(|n| doc.resolve(n)).and_then(|n| n.as_array().cloned()) {
        for pair in names.chunks(2) {
            if let [k, v] = pair
                && let Some(k) = doc.resolve(k).as_string()
                && out.len() < MAX_TREE_ENTRIES
            {
                out.push((k.bytes.clone(), v.clone()));
            }
        }
    }
    for k in d.get(b"Kids").map(|k| doc.resolve(k)).and_then(|k| k.as_array().cloned()).unwrap_or_default() {
        name_tree(doc, &k, depth + 1, seen, out);
    }
}

/// Two values (in two documents) are structurally the same, references followed. Streams never
/// compare equal; a value too deep or too large to compare is "different".
fn same_value(a_doc: &Document, a: &Object, b_doc: &Document, b: &Object, depth: usize, budget: &mut usize) -> bool {
    if depth > MAX_COMPARE_DEPTH || *budget == 0 {
        return false;
    }
    *budget -= 1;
    let (a, b) = (a_doc.resolve(a), b_doc.resolve(b));
    match (&*a, &*b) {
        (Object::Dict(x), Object::Dict(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same_value(a_doc, v, b_doc, w, depth + 1, budget)))
        }
        (Object::Array(x), Object::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(v, w)| same_value(a_doc, v, b_doc, w, depth + 1, budget)),
        (Object::Stream(_), _) | (_, Object::Stream(_)) | (Object::Ref(_), _) | (_, Object::Ref(_)) => false,
        (x, y) => x == y,
    }
}

/// The source role-map entries that conflict with the destination's: (type, kept, ignored).
fn role_conflicts(dst: &Document, dst_map: &Dict, src: &Document, src_map: &Dict) -> Vec<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    for (k, v) in src_map.iter() {
        let Some(theirs) = src.resolve(v).as_name().map(<[u8]>::to_vec) else { continue };
        let Some(ours) = dst_map.get(k).map(|o| dst.resolve(o)).and_then(|o| o.as_name().map(<[u8]>::to_vec)) else { continue };
        if ours != theirs {
            out.push((k.clone(), ours, theirs));
        }
    }
    out
}

/// What merging `src` into `dst` would report, without changing anything (Insert Pages from
/// File reports this): which sides are tagged, the role-map conflicts, and whether the result
/// would be marked tagged.
pub fn tag_merge_preview(dst: &Document, src: &Document) -> TagMergeReport {
    let mut report = TagMergeReport::default();
    let had_pages = crate::page_count(dst).map(|n| n > 0).unwrap_or(false);
    let (dst_tagged, src_tagged) = (is_tagged(dst), is_tagged(src));
    let (dst_root, src_root) = (tree_root(dst), tree_root(src));
    for t in [had_pages.then_some(dst_tagged), Some(src_tagged)].into_iter().flatten() {
        if t {
            report.tagged_sources += 1;
        } else {
            report.untagged_sources += 1;
        }
    }
    report.tree = dst_root.is_some() || src_root.is_some();
    if !report.tree {
        return report;
    }
    report.marked = (dst_tagged || !had_pages) && src_tagged;
    report.partially_tagged = !report.marked;
    if let (Some(d), Some(s)) = (&dst_root, &src_root) {
        let dm = resolved_dict(dst, d.get(b"RoleMap")).unwrap_or_default();
        let sm = resolved_dict(src, s.get(b"RoleMap")).unwrap_or_default();
        for (role, kept, ignored) in role_conflicts(dst, &dm, src, &sm) {
            let c =
                RoleMapConflict { role: name_text(&role), kept: name_text(&kept), ignored: name_text(&ignored), source: "the inserted pages".into() };
            report.warn(format!("role map: /{} maps to /{} here and to /{} in {}; kept /{}", c.role, c.kept, c.ignored, c.source, c.kept));
            report.role_map_conflicts.push(c);
        }
    }
    if report.partially_tagged {
        report.warn("the result would be partially tagged: part of it comes from an untagged document".into());
    }
    report
}

/// Merge the part of `src`'s structure tree on the copied pages into `dst` (without touching
/// `/MarkInfo`; see [`Tally`]). `label` names the source in warnings.
pub(crate) fn merge_source(
    dst: &mut Document,
    src: &Document,
    label: &str,
    imported: &Imported,
    before: &Before,
    report: &mut TagMergeReport,
) -> Result<(), OrganizeError> {
    let Some(src_root) = tree_root(src) else { return Ok(()) };
    let tree = read_tree(src, src_root);
    if tree.truncated {
        report.truncated = true;
        report.warn(format!("{label}: the structure tree is too large or too deep; only part of it was merged"));
    }
    let whole = crate::page_count(src).map(|n| n == imported.page_map.len()).unwrap_or(false);
    let src_role_map = resolved_dict(src, tree.root.get(b"RoleMap")).unwrap_or_default();

    // Which elements to keep: those with content on a copied page (computed bottom-up: kids
    // always come after their parent).
    let content_kept = |k: &Kid| match k {
        Kid::Content { page, stm: Some(s), mcid, .. } => {
            (0..=MAX_MCID).contains(mcid) && imported.objects.contains_key(s) && page.is_none_or(|p| imported.page_map.contains_key(&p))
        }
        Kid::Content { page, mcid, .. } => (0..=MAX_MCID).contains(mcid) && page.is_some_and(|p| imported.page_map.contains_key(&p)),
        Kid::Object { obj, .. } => imported.objects.contains_key(obj),
        Kid::Element(_) => false,
    };
    let n = tree.elems.len();
    let mut kept = vec![false; n];
    let mut has_content = vec![false; n];
    for i in (0..n).rev() {
        let Some(e) = tree.elems.get(i) else { continue };
        let (mut content, mut any) = (false, false);
        for k in &e.kids {
            match k {
                Kid::Element(j) => {
                    content |= has_content.get(*j).copied().unwrap_or(false);
                    any |= kept.get(*j).copied().unwrap_or(false);
                }
                other => {
                    content = true;
                    any |= content_kept(other);
                }
            }
        }
        let own_page = e.dict.get(b"Pg").and_then(Object::as_ref);
        let empty_kept = !content && own_page.map_or(whole, |p| imported.page_map.contains_key(&p));
        if let Some(h) = has_content.get_mut(i) {
            *h = content;
        }
        if let Some(k) = kept.get_mut(i) {
            *k = any || empty_kept;
        }
    }

    // A source whose single top-level element is a plain Document is dissolved into the
    // destination's Document element.
    let dissolve = match tree.roots.as_slice() {
        [only] => tree.elems.get(*only).filter(|e| {
            let plain = e.dict.iter().all(|(k, _)| matches!(k.as_slice(), b"Type" | b"S" | b"P" | b"K" | b"Pg" | b"Lang" | b"NS"));
            plain
                && e.dict.name(b"S").is_some_and(|s| maps_to(src, &src_role_map, s, b"Document"))
                && e.kids.iter().all(|k| matches!(k, Kid::Element(_)))
        }),
        _ => None,
    }
    .and(tree.roots.first().copied());
    let top: Vec<usize> = match dissolve.and_then(|d| tree.elems.get(d)) {
        Some(d) => d.kids.iter().filter_map(|k| if let Kid::Element(j) = k { Some(*j) } else { None }).collect(),
        None => tree.roots.clone(),
    };
    let top: Vec<usize> = top.into_iter().filter(|i| kept.get(*i).copied().unwrap_or(false)).collect();
    let kept_total = kept.iter().filter(|k| **k).count();
    let count = kept_total.saturating_sub(usize::from(dissolve.is_some_and(|d| kept.get(d).copied().unwrap_or(false))));
    report.elements_dropped = report.elements_dropped.saturating_add(n.saturating_sub(kept_total));
    if top.is_empty() {
        return Ok(());
    }

    // A new document takes the language of its first tagged source.
    if !before.had_pages
        && tree_root(dst).is_none()
        && doc_lang(dst).is_none()
        && let Some(lang) = doc_lang(src)
    {
        let (cat, _) = catalog(dst).ok_or(OrganizeError::NoPageTree)?;
        dst.update_dict(cat, |c| c.set(b"Lang".to_vec(), Object::String(lang)))?;
    }
    let root_ref = ensure_root(dst)?;
    let doc_elem = ensure_document(dst, root_ref)?;
    let dst_root = dst.get(root_ref).as_dict().cloned().unwrap_or_default();

    // The language that the moved top-level elements need to keep their meaning.
    let src_lang = dissolve
        .and_then(|d| tree.elems.get(d))
        .and_then(|d| d.dict.get(b"Lang").map(|l| src.resolve(l)).and_then(|l| l.as_string().cloned()))
        .or_else(|| doc_lang(src));
    let carry_lang = src_lang.filter(|l| doc_lang(dst).as_ref() != Some(l));

    let mut copier = Copier::with_map(src, imported.objects.clone());
    // New objects for the kept elements, seeded so references between elements (and from
    // attributes) map to the copies; references to elements left out map to a null object.
    let mut new_ref: Vec<Option<ObjRef>> = vec![None; n];
    let mut dead: Option<ObjRef> = None;
    for (i, e) in tree.elems.iter().enumerate() {
        if Some(i) == dissolve {
            if let Some(o) = e.obj {
                copier.seed(o, doc_elem);
            }
            continue;
        }
        if kept.get(i).copied().unwrap_or(false) {
            let r = dst.add(Object::Null);
            if let Some(slot) = new_ref.get_mut(i) {
                *slot = Some(r);
            }
            if let Some(o) = e.obj {
                copier.seed(o, r);
            }
        } else if let Some(o) = e.obj {
            let d = *dead.get_or_insert_with(|| dst.add(Object::Null));
            copier.seed(o, d);
        }
    }
    if let Some(r) = src.root().and_then(|c| src.get(c).as_dict().and_then(|c| c.reference(b"StructTreeRoot"))) {
        copier.seed(r, root_ref);
    }
    // Pages are never copied through an element's attributes: a copied page is its copy, any
    // other page is nothing (copying it would drag in the source's page tree).
    for (page, _) in crate::walk(src).unwrap_or_default() {
        match imported.page_map.get(&page) {
            Some(new) => copier.seed(page, *new),
            None => {
                let d = *dead.get_or_insert_with(|| dst.add(Object::Null));
                copier.seed(page, d);
            }
        }
    }
    let top_set: HashSet<usize> = top.iter().copied().collect();
    let src_index: HashMap<ObjRef, usize> = tree.elems.iter().enumerate().filter_map(|(i, e)| e.obj.map(|o| (o, i))).collect();

    // Role map, class map and namespaces.
    let mut role_map = resolved_dict(dst, dst_root.get(b"RoleMap")).unwrap_or_default();
    for (role, kept_as, ignored) in role_conflicts(dst, &role_map, src, &src_role_map) {
        let c = RoleMapConflict { role: name_text(&role), kept: name_text(&kept_as), ignored: name_text(&ignored), source: label.to_string() };
        report.warn(format!("role map: /{} maps to /{} in the result and to /{} in {}; kept /{}", c.role, c.kept, c.ignored, c.source, c.kept));
        report.role_map_conflicts.push(c);
    }
    for (k, v) in src_role_map.iter() {
        if !role_map.contains(k)
            && let Some(name) = src.resolve(v).as_name()
        {
            role_map.set(k.clone(), Object::Name(name.to_vec()));
        }
    }
    let class_renames = merge_class_map(dst, src, &tree.root, &dst_root, &mut copier, report)?;
    let namespaces = merge_namespaces(dst, src, &tree.root, &dst_root, &mut copier, report);

    // IDs.
    let mut ids: Vec<(Vec<u8>, Object)> = Vec::new();
    if let Some(t) = dst_root.get(b"IDTree") {
        name_tree(dst, t, 0, &mut HashSet::new(), &mut ids);
    }
    let mut used: HashSet<Vec<u8>> = ids.iter().map(|(k, _)| k.clone()).collect();

    // Parent tree contents: marked content per page and per stream, and object references.
    let mut on_page: HashMap<ObjRef, BTreeMap<i64, ObjRef>> = HashMap::new();
    let mut on_stream: BTreeMap<ObjRef, BTreeMap<i64, ObjRef>> = BTreeMap::new();
    let mut objects: Vec<(ObjRef, ObjRef)> = Vec::new();
    let mut seen_objects: HashSet<ObjRef> = HashSet::new();

    let map_page = |p: Option<ObjRef>| p.and_then(|p| imported.page_map.get(&p).copied());
    for (i, e) in tree.elems.iter().enumerate() {
        let Some(me) = new_ref.get(i).copied().flatten() else { continue };
        let mut d = Dict::new();
        for (k, v) in e.dict.iter() {
            match k.as_slice() {
                b"P" | b"K" | b"Pg" | b"ID" | b"Ref" => {}
                b"C" => d.set(k.clone(), rename_classes(src, v, &class_renames)),
                _ => d.set(k.clone(), copier.copy_value(dst, v, None)),
            }
        }
        let own_page = e.dict.get(b"Pg").and_then(Object::as_ref);
        let my_page = map_page(own_page);
        if let Some(p) = my_page {
            d.set(b"Pg".to_vec(), Object::Ref(p));
        }
        if let Some(id) = e.dict.get(b"ID").map(|o| src.resolve(o)).and_then(|o| o.as_string().map(|s| s.bytes.clone())) {
            let mut unique = id.clone();
            let mut k = 2usize;
            while used.contains(&unique) && k <= used.len().saturating_add(2) {
                unique = [id.as_slice(), format!("-{k}").as_bytes()].concat();
                k += 1;
            }
            if unique != id {
                report.ids_renamed.push((name_text(&id), name_text(&unique)));
            }
            used.insert(unique.clone());
            ids.push((unique.clone(), Object::Ref(me)));
            d.set(b"ID".to_vec(), Object::String(PdfString::literal(unique)));
        }
        if let Some(refs) = e.dict.get(b"Ref").map(|o| src.resolve(o)).and_then(|o| o.as_array().cloned()) {
            let mapped: Vec<Object> = refs
                .iter()
                .filter_map(Object::as_ref)
                .filter_map(|r| src_index.get(&r).and_then(|j| new_ref.get(*j).copied().flatten()))
                .map(Object::Ref)
                .collect();
            if !mapped.is_empty() {
                d.set(b"Ref".to_vec(), Object::Array(mapped));
            }
        }
        let is_top = top_set.contains(&i);
        let parent = if is_top { Some(doc_elem) } else { e.parent.and_then(|p| new_ref.get(p).copied().flatten()) };
        d.set(b"P".to_vec(), Object::Ref(parent.unwrap_or(doc_elem)));
        if is_top
            && !d.contains(b"Lang")
            && let Some(l) = &carry_lang
        {
            d.set(b"Lang".to_vec(), Object::String(l.clone()));
        }
        // Kids.
        let mut kids = Vec::new();
        for k in &e.kids {
            match k {
                Kid::Element(j) => {
                    if let Some(r) = new_ref.get(*j).copied().flatten() {
                        kids.push(Object::Ref(r));
                    }
                }
                Kid::Content { page, mcid, stm, stm_own } => {
                    if !content_kept(k) {
                        continue;
                    }
                    let new_page = map_page(*page);
                    match stm.and_then(|s| copier.copied(s)) {
                        Some(new_stm) => {
                            let mut mcr = Dict::new();
                            mcr.set(b"Type".to_vec(), Object::name("MCR"));
                            if let Some(p) = new_page {
                                mcr.set(b"Pg".to_vec(), Object::Ref(p));
                            }
                            mcr.set(b"Stm".to_vec(), Object::Ref(new_stm));
                            if let Some(own) = stm_own {
                                mcr.set(b"StmOwn".to_vec(), copier.copy_value(dst, own, None));
                            }
                            mcr.set(b"MCID".to_vec(), Object::Int(*mcid));
                            kids.push(Object::Dict(mcr));
                            on_stream.entry(new_stm).or_default().insert(*mcid, me);
                        }
                        None => {
                            let Some(p) = new_page else { continue };
                            if Some(p) == my_page {
                                kids.push(Object::Int(*mcid));
                            } else {
                                let mut mcr = Dict::new();
                                mcr.set(b"Type".to_vec(), Object::name("MCR"));
                                mcr.set(b"Pg".to_vec(), Object::Ref(p));
                                mcr.set(b"MCID".to_vec(), Object::Int(*mcid));
                                kids.push(Object::Dict(mcr));
                            }
                            on_page.entry(p).or_default().insert(*mcid, me);
                        }
                    }
                }
                Kid::Object { page, obj } => {
                    let Some(new_obj) = copier.copied(*obj) else { continue };
                    let mut o = Dict::new();
                    o.set(b"Type".to_vec(), Object::name("OBJR"));
                    if let Some(p) = map_page(*page) {
                        o.set(b"Pg".to_vec(), Object::Ref(p));
                    }
                    o.set(b"Obj".to_vec(), Object::Ref(new_obj));
                    kids.push(Object::Dict(o));
                    if seen_objects.insert(new_obj) {
                        objects.push((new_obj, me));
                    }
                }
            }
        }
        match kids.len() {
            0 => {}
            1 => {
                if let Some(only) = kids.pop() {
                    d.set(b"K".to_vec(), only);
                }
            }
            _ => d.set(b"K".to_vec(), Object::Array(kids)),
        }
        if !d.contains(b"Type") {
            d.set(b"Type".to_vec(), Object::name("StructElem"));
        }
        dst.set(me, Object::Dict(d));
    }
    if let Some(d) = dead {
        dst.free(d);
    }
    report.elements = report.elements.saturating_add(count);

    // The new top-level elements go among the Document element's kids in page order.
    let new_top: Vec<Object> = top.iter().filter_map(|i| new_ref.get(*i).copied().flatten()).map(Object::Ref).collect();
    insert_top(dst, doc_elem, new_top, &imported.pages)?;

    // The parent tree.
    write_parent_tree(dst, root_ref, &imported.pages, on_page, on_stream, objects, report)?;

    // The root's maps.
    let id_tree = if ids.is_empty() {
        None
    } else {
        ids.sort_by(|a, b| a.0.cmp(&b.0));
        let mut names = Vec::with_capacity(ids.len().saturating_mul(2));
        for (k, v) in ids {
            names.push(Object::String(PdfString::literal(k)));
            names.push(v);
        }
        let mut t = Dict::new();
        t.set(b"Names".to_vec(), Object::Array(names));
        Some(dst.add(t))
    };
    dst.update_dict(root_ref, |r| {
        if !role_map.is_empty() {
            r.set(b"RoleMap".to_vec(), Object::Dict(role_map));
        }
        if let Some(t) = id_tree {
            r.set(b"IDTree".to_vec(), Object::Ref(t));
        }
        if let Some(ns) = namespaces {
            r.set(b"Namespaces".to_vec(), Object::Array(ns));
        }
    })?;
    Ok(())
}

/// The destination's `/StructTreeRoot` as an indirect object (created when missing).
fn ensure_root(dst: &mut Document) -> Result<ObjRef, OrganizeError> {
    let (cat, c) = catalog(dst).ok_or(OrganizeError::NoPageTree)?;
    if let Some(r) = c.reference(b"StructTreeRoot")
        && dst.get(r).as_dict().is_some()
    {
        return Ok(r);
    }
    let mut root = match c.get(b"StructTreeRoot") {
        Some(Object::Dict(d)) => d.clone(),
        _ => Dict::new(),
    };
    root.set(b"Type".to_vec(), Object::name("StructTreeRoot"));
    let r = dst.add(root);
    dst.update_dict(cat, |c| c.set(b"StructTreeRoot".to_vec(), Object::Ref(r)))?;
    Ok(r)
}

/// The destination's one top-level Document element: the existing one, or a new one holding
/// whatever was at the top level before.
fn ensure_document(dst: &mut Document, root_ref: ObjRef) -> Result<ObjRef, OrganizeError> {
    let root = dst.get(root_ref).as_dict().cloned().unwrap_or_default();
    let role_map = resolved_dict(dst, root.get(b"RoleMap")).unwrap_or_default();
    let k = root.get(b"K").cloned();
    let single = match &k {
        Some(Object::Array(a)) if a.len() == 1 => a.first().cloned(),
        Some(o) => Some(o.clone()),
        None => None,
    };
    let is_document = |d: &Dict| d.name(b"S").is_some_and(|s| maps_to(dst, &role_map, s, b"Document"));
    match single {
        Some(Object::Ref(r)) if dst.get(r).as_dict().is_some_and(is_document) => return Ok(r),
        Some(Object::Dict(d)) if is_document(&d) => {
            let mut d = d;
            d.set(b"P".to_vec(), Object::Ref(root_ref));
            let r = dst.add(d);
            dst.update_dict(root_ref, |x| x.set(b"K".to_vec(), Object::Ref(r)))?;
            return Ok(r);
        }
        _ => {}
    }
    let old: Vec<Object> = match k {
        Some(Object::Array(a)) => a,
        Some(Object::Null) | None => Vec::new(),
        Some(o) => vec![o],
    };
    let mut d = Dict::new();
    d.set(b"Type".to_vec(), Object::name("StructElem"));
    d.set(b"S".to_vec(), Object::name("Document"));
    d.set(b"P".to_vec(), Object::Ref(root_ref));
    d.set(b"K".to_vec(), Object::Array(old.clone()));
    let r = dst.add(d);
    for o in old.iter().filter_map(Object::as_ref) {
        if dst.get(o).as_dict().is_some_and(|d| d.contains(b"S")) {
            dst.update_dict(o, |e| e.set(b"P".to_vec(), Object::Ref(r)))?;
        }
    }
    dst.update_dict(root_ref, |x| x.set(b"K".to_vec(), Object::Ref(r)))?;
    Ok(r)
}

/// The smallest page index (in `index`) that an element's subtree shows content on, within
/// `budget` visited objects. `inherited` is the page of the nearest ancestor with a `/Pg`.
fn first_page(doc: &Document, kid: &Object, inherited: Option<ObjRef>, index: &HashMap<ObjRef, usize>, budget: &mut usize) -> Option<usize> {
    let mut best: Option<usize> = None;
    let mut note = |p: Option<ObjRef>| {
        if let Some(i) = p.and_then(|p| index.get(&p).copied()) {
            best = Some(best.map_or(i, |b: usize| b.min(i)));
        }
    };
    let mut stack = vec![(kid.clone(), inherited, 0usize)];
    let mut seen = HashSet::new();
    let mut local = 10_000usize;
    while let Some((o, page, depth)) = stack.pop() {
        if *budget == 0 || local == 0 || depth > 64 {
            break;
        }
        *budget -= 1;
        local -= 1;
        let o = match o {
            Object::Ref(r) => {
                if !seen.insert(r) {
                    continue;
                }
                (*doc.get(r)).clone()
            }
            other => other,
        };
        match o {
            Object::Array(a) => stack.extend(a.into_iter().map(|x| (x, page, depth + 1))),
            Object::Int(_) => note(page),
            Object::Dict(d) => {
                let page = d.get(b"Pg").and_then(Object::as_ref).or(page);
                match d.name(b"Type") {
                    Some(b"MCR") | Some(b"OBJR") => note(page),
                    _ => {
                        if let Some(k) = d.get(b"K") {
                            stack.push((k.clone(), page, depth + 1));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    best
}

/// Insert `new_top` into the Document element's kids after the last kid whose content starts
/// before the copied `pages` end (scanning from the end, so appending is cheap).
fn insert_top(dst: &mut Document, doc_elem: ObjRef, new_top: Vec<Object>, pages: &[ObjRef]) -> Result<(), OrganizeError> {
    let order = crate::walk(dst)?;
    let index: HashMap<ObjRef, usize> = order.iter().enumerate().map(|(i, (r, _))| (*r, i)).collect();
    let after = pages.iter().filter_map(|p| index.get(p).copied()).max().map_or(usize::MAX, |m| m.saturating_add(1));
    let d = dst.get(doc_elem).as_dict().cloned().unwrap_or_default();
    let mut kids: Vec<Object> = match d.get(b"K").cloned() {
        Some(Object::Array(a)) => a,
        Some(Object::Null) | None => Vec::new(),
        Some(o) => vec![o],
    };
    let inherited = d.get(b"Pg").and_then(Object::as_ref);
    let mut budget = 1_000_000usize;
    let (mut at, mut later_seen) = (None, false);
    for (i, k) in kids.iter().enumerate().rev() {
        if budget == 0 {
            break;
        }
        match first_page(dst, k, inherited, &index, &mut budget) {
            Some(p) if p < after => {
                at = Some(i + 1);
                break;
            }
            Some(_) => later_seen = true,
            None => {}
        }
    }
    // Nothing starts before the new pages: they go first, unless no kid's page is known.
    let at = at.unwrap_or(if later_seen && budget > 0 { 0 } else { kids.len() }).min(kids.len());
    kids.splice(at..at, new_top);
    dst.update_dict(doc_elem, |e| e.set(b"K".to_vec(), Object::Array(kids)))?;
    Ok(())
}

/// Add the copied pages', streams' and objects' entries to the parent tree under new keys,
/// writing it back as one flat number tree.
fn write_parent_tree(
    dst: &mut Document,
    root_ref: ObjRef,
    pages: &[ObjRef],
    mut on_page: HashMap<ObjRef, BTreeMap<i64, ObjRef>>,
    on_stream: BTreeMap<ObjRef, BTreeMap<i64, ObjRef>>,
    objects: Vec<(ObjRef, ObjRef)>,
    report: &mut TagMergeReport,
) -> Result<(), OrganizeError> {
    let root = dst.get(root_ref).as_dict().cloned().unwrap_or_default();
    let mut entries: Vec<(i64, Object)> = Vec::new();
    if let Some(pt) = root.get(b"ParentTree") {
        number_tree(dst, pt, 0, &mut HashSet::new(), &mut entries);
    }
    let mut next = root.get(b"ParentTreeNextKey").map(|o| dst.resolve(o)).and_then(|o| o.as_int()).unwrap_or(0).max(0);
    if let Some(m) = entries.iter().map(|(k, _)| *k).max() {
        next = next.max(m.saturating_add(1));
    }
    if entries.len() >= MAX_TREE_ENTRIES {
        report.truncated = true;
        report.warn("the parent tree is too large to read in full; entries beyond the limit were lost".into());
    }
    let mut slots = 0usize;
    let mut keyed: Vec<(ObjRef, i64, &'static [u8])> = Vec::new();
    let mut overflow = false;
    for p in pages {
        let Some(owners) = on_page.remove(p) else { continue };
        let Some(k) = take_key(&mut next) else {
            overflow = true;
            break;
        };
        entries.push((k, mcid_array(&owners, &mut slots, report)));
        keyed.push((*p, k, b"StructParents"));
    }
    for (s, owners) in &on_stream {
        let Some(k) = take_key(&mut next) else {
            overflow = true;
            break;
        };
        entries.push((k, mcid_array(owners, &mut slots, report)));
        keyed.push((*s, k, b"StructParents"));
    }
    for (o, owner) in &objects {
        let Some(k) = take_key(&mut next) else {
            overflow = true;
            break;
        };
        entries.push((k, Object::Ref(*owner)));
        keyed.push((*o, k, b"StructParent"));
    }
    if overflow {
        report.truncated = true;
        report.warn("the parent tree ran out of keys: some content was left out of it".into());
    }
    for (r, k, key) in keyed {
        if dst.get(r).as_dict().is_some() {
            dst.update_dict(r, |d| d.set(key.to_vec(), Object::Int(k)))?;
        }
    }
    entries.sort_by_key(|(k, _)| *k);
    entries.dedup_by_key(|(k, _)| *k);
    let mut nums = Vec::with_capacity(entries.len().saturating_mul(2));
    for (k, v) in entries {
        nums.push(Object::Int(k));
        nums.push(v);
    }
    let mut t = Dict::new();
    t.set(b"Nums".to_vec(), Object::Array(nums));
    let pt = dst.add(t);
    dst.update_dict(root_ref, |r| {
        r.set(b"ParentTree".to_vec(), Object::Ref(pt));
        r.set(b"ParentTreeNextKey".to_vec(), Object::Int(next));
    })?;
    Ok(())
}

/// The next parent-tree key, or `None` when they ran out.
fn take_key(next: &mut i64) -> Option<i64> {
    let k = *next;
    *next = next.checked_add(1)?;
    Some(k)
}

/// A page's or stream's parent-tree array: the owner of each marked-content ID, by ID.
fn mcid_array(owners: &BTreeMap<i64, ObjRef>, slots: &mut usize, report: &mut TagMergeReport) -> Object {
    let len = owners.keys().next_back().map_or(0, |m| usize::try_from(*m).unwrap_or(usize::MAX).saturating_add(1));
    if slots.saturating_add(len) > MAX_PARENT_SLOTS {
        report.truncated = true;
        report.warn("marked-content IDs too large to index: some content was left out of the parent tree".into());
        return Object::Array(Vec::new());
    }
    *slots += len;
    let mut a = vec![Object::Null; len];
    for (m, owner) in owners {
        if let Some(slot) = usize::try_from(*m).ok().and_then(|m| a.get_mut(m)) {
            *slot = Object::Ref(*owner);
        }
    }
    Object::Array(a)
}

/// Merge the source's `/ClassMap` into the destination's. Returns the classes renamed for this
/// source (old → new).
fn merge_class_map(
    dst: &mut Document,
    src: &Document,
    src_root: &Dict,
    dst_root: &Dict,
    copier: &mut Copier<'_>,
    report: &mut TagMergeReport,
) -> Result<HashMap<Vec<u8>, Vec<u8>>, OrganizeError> {
    let mut renames = HashMap::new();
    let Some(theirs) = resolved_dict(src, src_root.get(b"ClassMap")) else { return Ok(renames) };
    let mut ours = resolved_dict(dst, dst_root.get(b"ClassMap")).unwrap_or_default();
    let mut changed = false;
    for (name, value) in theirs.iter() {
        let existing = ours.get(name).cloned();
        let target = match existing {
            None => name.clone(),
            Some(v) if same_value(src, value, dst, &v, 0, &mut MAX_COMPARE_NODES.clone()) => continue,
            Some(_) => {
                let mut n = 2usize;
                let mut candidate = [name.as_slice(), format!("_{n}").as_bytes()].concat();
                while ours.contains(&candidate) && n <= ours.len().saturating_add(2) {
                    n += 1;
                    candidate = [name.as_slice(), format!("_{n}").as_bytes()].concat();
                }
                report.classes_renamed.push((name_text(name), name_text(&candidate)));
                renames.insert(name.clone(), candidate.clone());
                candidate
            }
        };
        let copied = copier.copy_value(dst, value, None);
        ours.set(target, copied);
        changed = true;
    }
    if changed {
        let root = dst.get(dst.root().ok_or(OrganizeError::NoPageTree)?).as_dict().and_then(|c| c.reference(b"StructTreeRoot"));
        if let Some(r) = root {
            dst.update_dict(r, |x| x.set(b"ClassMap".to_vec(), Object::Dict(ours)))?;
        }
    }
    Ok(renames)
}

/// An element's `/C` (a class name, or an array of names and revision numbers) with this
/// source's renamed classes.
fn rename_classes(src: &Document, c: &Object, renames: &HashMap<Vec<u8>, Vec<u8>>) -> Object {
    let rename = |o: &Object| match o {
        Object::Name(n) => Object::Name(renames.get(n).cloned().unwrap_or_else(|| n.clone())),
        other => other.clone(),
    };
    match &*src.resolve(c) {
        Object::Array(a) => Object::Array(a.iter().map(rename).collect()),
        other => rename(other),
    }
}

/// Merge the source's PDF 2.0 `/Namespaces` into the destination's: a namespace with the same
/// URI, schema and role mapping as one already there is shared, the others are copied. Returns
/// the new `/Namespaces` array when it changed.
fn merge_namespaces(
    dst: &mut Document,
    src: &Document,
    src_root: &Dict,
    dst_root: &Dict,
    copier: &mut Copier<'_>,
    report: &mut TagMergeReport,
) -> Option<Vec<Object>> {
    let theirs = src_root.get(b"Namespaces").map(|n| src.resolve(n)).and_then(|n| n.as_array().cloned())?;
    let mut ours = dst_root.get(b"Namespaces").map(|n| dst.resolve(n)).and_then(|n| n.as_array().cloned()).unwrap_or_default();
    // First match the shared ones, so copies of the others point at them.
    let mut to_copy = Vec::new();
    for ns in theirs.iter().take(MAX_NAMESPACES) {
        let Some(r) = ns.as_ref() else {
            report.warn("a namespace in /Namespaces is not an indirect object; it was left out".into());
            continue;
        };
        let shared = ours.iter().filter_map(Object::as_ref).find(|o| same_value(src, ns, dst, &Object::Ref(*o), 0, &mut MAX_COMPARE_NODES.clone()));
        match shared {
            Some(o) => copier.seed(r, o),
            None => to_copy.push(r),
        }
    }
    let mut changed = false;
    for r in to_copy {
        if copier.copied(r).is_some() {
            continue;
        }
        let new = copier.copy_ref(dst, r);
        ours.push(Object::Ref(new));
        report.namespaces_added += 1;
        changed = true;
    }
    changed.then_some(ours)
}

/// Namespaces read from one `/Namespaces` array.
pub(crate) const MAX_NAMESPACES: usize = 1_000;

#[cfg(test)]
#[path = "structure_tests.rs"]
mod tests;
