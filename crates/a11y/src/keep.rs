//! Keep tags on edit: after an edit changes a tagged page's content, bring the structure tree
//! back in step with it, as Acrobat does when text and images are edited in a tagged PDF.
//!
//! The caller takes a [`Snapshot`] of the page before the edit and calls [`keep_tags`] after it.
//! The page's marked content is compared before and after:
//!
//! - Content that changed inside its marked-content sequence keeps its MCID and element. An
//!   element whose text changed loses its `/ActualText` (it spelled out the old words); a figure
//!   whose image changed keeps its `/Alt` and is reported for review.
//! - New content outside any marked content (a rewritten paragraph, added text or images) takes
//!   over the MCID of content of the same kind that the edit emptied (the replaced paragraph or
//!   image), so it stays in the same element and reading-order position. Otherwise it gets a new
//!   MCID in a new element (text: `P`, images: `Figure`), placed next to the element of the
//!   nearest tagged content on the page. New vector-only content (underlines) becomes an
//!   artifact.
//! - Marked content the edit emptied or removed leaves its element; an element left with no kids
//!   is removed from the tree.
//! - The page's parent-tree entry is rebuilt from the tree.
//!
//! [`pages_snapshot`] and [`drop_page_tags`] do the same for deleted pages: their marked content,
//! object references and parent-tree entries leave the tree.
//!
//! Every walk is bounded and cycle-safe; a document whose tree can't be updated keeps its edit and
//! gets a warning in the report.

use std::collections::{HashMap, HashSet};

use pdfcraft_content::{Op, Pieces};
use pdfcraft_cos::{Dict, Document, ObjRef, Object, Stream};

use crate::tags::{MAX_MCID, TagError, TagKid, Tags, catalog, sub_dict, tags};

/// Pages with more operators than this are not compared (reported instead).
const MAX_OPS: usize = 2_000_000;
/// Marked-content nesting deeper than this is not followed.
const MAX_NEST: usize = 4096;
/// Raw structure walks stop after this many objects.
const MAX_WALK: usize = 2_000_000;
/// Number-tree walks stop after this many nodes.
const MAX_TREE_NODES: usize = 100_000;
/// Ancestors followed when looking for a grouping element.
const MAX_CLIMB: usize = 512;

/// Element types that group block-level elements: new paragraphs and figures go inside them.
const GROUPING: &[&str] = &["Document", "DocumentFragment", "Part", "Art", "Sect", "Div", "BlockQuote", "Aside", "NonStruct"];

/// The kind of a drawing unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Text,
    Image,
    Path,
}

/// One drawing operation: a text-showing operator, an XObject or inline image, a shading, or a
/// painted path (from its first construction operator to its painting operator).
#[derive(Clone, Debug)]
struct Unit {
    start: usize,
    end: usize,
    class: Class,
    /// The innermost enclosing MCID.
    mcid: Option<i64>,
    /// Inside `/Artifact` marked content.
    artifact: bool,
    /// What it draws (its operators, and the objects its XObjects resolve to).
    sig: Vec<u8>,
    /// Changes at every BT, ET, q, Q and marked-content operator: units in one segment can share
    /// one marked-content sequence.
    seg: usize,
}

/// One marked-content sequence with an MCID.
#[derive(Clone, Debug)]
struct Seq {
    mcid: i64,
    bdc: usize,
    emc: Option<usize>,
    /// The BDC's operands (tag and properties), to re-create it.
    operands: Vec<Object>,
}

#[derive(Clone, Debug, Default)]
struct Scan {
    units: Vec<Unit>,
    seqs: Vec<Seq>,
    max_mcid: Option<i64>,
}

#[derive(Clone, Debug, Default)]
struct McInfo {
    drawing: bool,
    text: bool,
    image: bool,
    sig: Vec<u8>,
}

impl McInfo {
    fn class(&self) -> Class {
        if self.text {
            Class::Text
        } else if self.image {
            Class::Image
        } else {
            Class::Path
        }
    }
}

impl Scan {
    fn by_mcid(&self) -> HashMap<i64, McInfo> {
        let mut out: HashMap<i64, McInfo> = self.seqs.iter().map(|s| (s.mcid, McInfo::default())).collect();
        for u in &self.units {
            let Some(m) = u.mcid else { continue };
            let e = out.entry(m).or_default();
            e.drawing = true;
            e.text |= u.class == Class::Text;
            e.image |= u.class == Class::Image;
            e.sig.extend_from_slice(&u.sig);
        }
        out
    }
}

fn is_path_construction(op: &[u8]) -> bool {
    matches!(op, b"m" | b"l" | b"c" | b"v" | b"y" | b"h" | b"re")
}

fn is_path_paint(op: &[u8]) -> bool {
    matches!(op, b"S" | b"s" | b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*")
}

/// Read a page's operators: drawing units and MCID sequences.
fn scan(doc: &Document, page: &Dict, ops: &[Op]) -> Scan {
    let res = sub_dict(doc, page, b"Resources");
    let xobjects = sub_dict(doc, &res, b"XObject");
    let props = sub_dict(doc, &res, b"Properties");
    let mut out = Scan::default();
    // Open marked content: (MCID, artifact, index into out.seqs).
    let mut marks: Vec<(Option<i64>, bool, Option<usize>)> = Vec::new();
    let mut seen_mcids: HashSet<i64> = HashSet::new();
    let mut seg = 0usize;
    let mut path_start: Option<usize> = None;
    let push = |out: &mut Scan, marks: &[(Option<i64>, bool, Option<usize>)], start: usize, end: usize, class: Class, sig: Vec<u8>, seg: usize| {
        let mcid = marks.iter().rev().find_map(|m| m.0);
        let artifact = marks.iter().any(|m| m.1);
        out.units.push(Unit { start, end, class, mcid, artifact, sig, seg });
    };
    for (i, op) in ops.iter().enumerate() {
        let kw = op.op.as_slice();
        if is_path_construction(kw) {
            path_start.get_or_insert(i);
            continue;
        }
        if matches!(kw, b"W" | b"W*") {
            continue;
        }
        if is_path_paint(kw) {
            let start = path_start.take().unwrap_or(i);
            let mut sig = Vec::new();
            for o in ops.get(start..=i).unwrap_or_default() {
                pdfcraft_content::write_op(o, &mut sig);
            }
            push(&mut out, &marks, start, i, Class::Path, sig, seg);
            continue;
        }
        path_start = None;
        match kw {
            b"BT" | b"ET" | b"q" | b"Q" | b"BX" | b"EX" => seg = seg.saturating_add(1),
            b"BMC" => {
                seg = seg.saturating_add(1);
                if marks.len() < MAX_NEST {
                    marks.push((None, op.name(0) == Some(b"Artifact"), None));
                }
            }
            b"BDC" => {
                seg = seg.saturating_add(1);
                let p = match op.operands.get(1) {
                    Some(Object::Dict(d)) => Some(d.clone()),
                    Some(Object::Name(n)) => props.get(n).map(|x| doc.resolve(x)).and_then(|x| x.as_dict().cloned()),
                    _ => None,
                };
                let mcid = p.as_ref().and_then(|d| d.get(b"MCID").and_then(Object::as_int)).filter(|m| (0..=MAX_MCID).contains(m));
                let mut at = None;
                if let Some(m) = mcid {
                    out.max_mcid = Some(out.max_mcid.map_or(m, |x| x.max(m)));
                    // A repeated MCID is malformed: only its first sequence is followed.
                    if seen_mcids.insert(m) {
                        at = Some(out.seqs.len());
                        out.seqs.push(Seq { mcid: m, bdc: i, emc: None, operands: op.operands.clone() });
                    }
                }
                if marks.len() < MAX_NEST {
                    marks.push((mcid, op.name(0) == Some(b"Artifact"), at));
                }
            }
            b"EMC" => {
                seg = seg.saturating_add(1);
                if let Some((_, _, Some(at))) = marks.pop()
                    && let Some(s) = out.seqs.get_mut(at)
                {
                    s.emc = Some(i);
                }
            }
            b"Tj" | b"TJ" | b"'" | b"\"" => {
                let mut sig = Vec::new();
                pdfcraft_content::write_op(op, &mut sig);
                push(&mut out, &marks, i, i, Class::Text, sig, seg);
            }
            b"Do" => {
                let mut sig = Vec::new();
                pdfcraft_content::write_op(op, &mut sig);
                if let Some(Object::Ref(r)) = op.name(0).and_then(|n| xobjects.get(n)) {
                    sig.extend_from_slice(format!("@{} {}", r.num, r.generation).as_bytes());
                }
                push(&mut out, &marks, i, i, Class::Image, sig, seg);
            }
            b"BI" => {
                let mut sig = Vec::new();
                pdfcraft_content::write_op(op, &mut sig);
                push(&mut out, &marks, i, i, Class::Image, sig, seg);
            }
            b"sh" => {
                let mut sig = Vec::new();
                pdfcraft_content::write_op(op, &mut sig);
                push(&mut out, &marks, i, i, Class::Path, sig, seg);
            }
            _ => {}
        }
    }
    out
}

/// A page's content streams and their decoded bytes.
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

/// A tagged page as it was before an edit (see [`keep_tags`]).
#[derive(Clone, Debug)]
pub struct Snapshot {
    page: usize,
    obj: ObjRef,
    scan: Scan,
}

impl Snapshot {
    /// The 0-based page.
    pub fn page(&self) -> usize {
        self.page
    }
}

/// The page's marked content before an edit; `None` for an untagged document, a missing page,
/// or a page too large to compare.
pub fn snapshot(doc: &Document, page: usize) -> Option<Snapshot> {
    let (_, cat) = catalog(doc)?;
    cat.get(b"StructTreeRoot")?;
    let pg = pdfcraft_model::pages(doc).into_iter().nth(page)?;
    let streams = content_streams(doc, &pg.dict);
    let pieces = Pieces::join(&streams.iter().map(|(_, d)| d.as_slice()).collect::<Vec<_>>());
    let ops = pieces.parse().ops;
    if ops.len() > MAX_OPS {
        return None;
    }
    Some(Snapshot { page, obj: pg.obj, scan: scan(doc, &pg.dict, &ops) })
}

/// An element made for new content.
#[derive(Clone, Debug, PartialEq)]
pub struct AddedTag {
    /// 0-based page.
    pub page: usize,
    pub element: ObjRef,
    /// `P` or `Figure`.
    pub role: String,
    pub mcid: i64,
}

/// What keeping the tags in step did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TagUpdate {
    /// 0-based pages whose tags were updated (for deleted pages, where they were).
    pub pages: Vec<usize>,
    /// Marked-content sequences whose content changed and kept their tag.
    pub kept: usize,
    /// New content that took over the tag of the content it replaced.
    pub adopted: usize,
    /// Elements made for new content.
    pub added: Vec<AddedTag>,
    /// New decorations (vector-only content) marked as artifacts.
    pub artifacts: usize,
    /// Marked-content and object references removed from elements (content that went).
    pub removed: usize,
    /// Elements removed from the tree because they were left with nothing.
    pub pruned: usize,
    /// Elements and marked-content sequences whose `/ActualText` went with the text it spelled.
    pub actual_text_cleared: usize,
    /// Figures whose image changed: their alternate text was kept, but may describe the old one.
    pub review_alt: Vec<ObjRef>,
    /// Parent-tree entries rewritten or removed.
    pub parent_tree: usize,
    /// What could not be kept in step.
    pub warnings: Vec<String>,
}

impl TagUpdate {
    pub fn is_empty(&self) -> bool {
        *self == TagUpdate::default()
    }

    pub fn merge(&mut self, o: TagUpdate) {
        for p in o.pages {
            if !self.pages.contains(&p) {
                self.pages.push(p);
            }
        }
        self.kept = self.kept.saturating_add(o.kept);
        self.adopted = self.adopted.saturating_add(o.adopted);
        self.added.extend(o.added);
        self.artifacts = self.artifacts.saturating_add(o.artifacts);
        self.removed = self.removed.saturating_add(o.removed);
        self.pruned = self.pruned.saturating_add(o.pruned);
        self.actual_text_cleared = self.actual_text_cleared.saturating_add(o.actual_text_cleared);
        for r in o.review_alt {
            if !self.review_alt.contains(&r) {
                self.review_alt.push(r);
            }
        }
        self.parent_tree = self.parent_tree.saturating_add(o.parent_tree);
        self.warnings.extend(o.warnings);
    }

    fn touched(&self) -> bool {
        self.kept > 0
            || self.adopted > 0
            || !self.added.is_empty()
            || self.artifacts > 0
            || self.removed > 0
            || self.pruned > 0
            || self.actual_text_cleared > 0
            || !self.review_alt.is_empty()
    }
}

/// A run of new untagged units that goes into one marked-content sequence.
struct Run {
    start: usize,
    end: usize,
    class: Class,
}

enum Fate {
    /// Into the emptied sequence with this MCID.
    Adopt(i64),
    New(Class),
    Artifact,
}

/// The standard role of an element in the tree read, or of one this pass made.
struct Elements<'a> {
    tree: &'a Tags,
    index: HashMap<ObjRef, usize>,
    /// Elements made in this pass: (parent, role); `None` parent is the root.
    made: HashMap<ObjRef, (Option<ObjRef>, &'static str)>,
}

impl<'a> Elements<'a> {
    fn new(tree: &'a Tags) -> Self {
        let index = tree.nodes.iter().enumerate().filter_map(|(i, n)| n.obj.map(|o| (o, i))).collect();
        Elements { tree, index, made: HashMap::new() }
    }

    fn parent(&self, e: ObjRef) -> Option<Option<ObjRef>> {
        if let Some((p, _)) = self.made.get(&e) {
            return Some(*p);
        }
        let n = self.tree.nodes.get(*self.index.get(&e)?)?;
        match n.parent {
            None => Some(None),
            Some(p) => self.tree.nodes.get(p).and_then(|p| p.obj).map(Some),
        }
    }

    fn role(&self, e: ObjRef) -> Option<&str> {
        if let Some((_, r)) = self.made.get(&e) {
            return Some(r);
        }
        self.tree.nodes.get(*self.index.get(&e)?).map(|n| n.role())
    }

    fn grouping(&self, e: ObjRef) -> bool {
        self.role(e).is_some_and(|r| GROUPING.contains(&r))
    }
}

/// An element's kids as a list (K normalized: a single kid becomes a one-item list).
fn kid_list(doc: &Document, d: &Dict) -> Vec<Object> {
    match d.get(b"K") {
        None => Vec::new(),
        Some(Object::Array(a)) => a.clone(),
        Some(Object::Ref(r)) => match &*doc.get(*r) {
            Object::Array(a) => a.clone(),
            _ => vec![Object::Ref(*r)],
        },
        Some(o) => vec![o.clone()],
    }
}

/// Where the tree's root and parent tree are (the root must be an indirect dictionary).
fn root_ref(doc: &Document) -> Result<ObjRef, TagError> {
    let (_, cat) = catalog(doc).ok_or(TagError::Untagged)?;
    match cat.get(b"StructTreeRoot") {
        Some(Object::Ref(r)) if doc.get(*r).as_dict().is_some() => Ok(*r),
        Some(_) => Err(TagError::Invalid("the structure tree root is not an indirect dictionary; its tags were not updated".into())),
        None => Err(TagError::Untagged),
    }
}

/// Insert `new` into `parent`'s kids (`None`: the root's) after (or before) `anchor`, or at the
/// end (start) when the anchor isn't one of them.
fn insert_kid(doc: &mut Document, root: ObjRef, parent: Option<ObjRef>, anchor: Option<ObjRef>, after: bool, new: ObjRef) -> Result<(), TagError> {
    let holder = parent.unwrap_or(root);
    let d = doc.get(holder).as_dict().cloned().ok_or_else(|| TagError::Invalid("a structure element is not a dictionary".into()))?;
    let mut list = kid_list(doc, &d);
    let at = anchor.and_then(|a| list.iter().position(|k| k.as_ref() == Some(a)));
    let pos = match (at, after) {
        (Some(i), true) => i.saturating_add(1),
        (Some(i), false) => i,
        (None, true) => list.len(),
        (None, false) => 0,
    };
    list.insert(pos.min(list.len()), Object::Ref(new));
    doc.update_dict(holder, |d| d.set(b"K".to_vec(), Object::Array(list)))?;
    Ok(())
}

/// A marked-content reference on `page_obj` (inheriting `here` when it names no page), unless
/// it is in another stream.
fn mcr_mcid(doc: &Document, o: &Object, here: bool, page_obj: ObjRef) -> Option<i64> {
    let resolved;
    let d = match o {
        Object::Int(n) => return here.then_some(*n),
        Object::Dict(d) => d,
        Object::Ref(r) => {
            resolved = doc.get(*r);
            resolved.as_dict()?
        }
        _ => return None,
    };
    if d.name(b"Type") != Some(b"MCR") || d.contains(b"Stm") {
        return None;
    }
    let on_page = d.get(b"Pg").and_then(Object::as_ref).map_or(here, |p| p == page_obj);
    on_page.then(|| d.get(b"MCID").and_then(Object::as_int)).flatten()
}

/// Remove the MCIDs in `gone` from element `e`'s kids. Returns (removed, now empty).
fn strip_mcids(doc: &mut Document, e: ObjRef, here: bool, page_obj: ObjRef, gone: &HashSet<i64>) -> Result<(usize, bool), TagError> {
    let Some(d) = doc.get(e).as_dict().cloned() else { return Ok((0, false)) };
    let here = d.get(b"Pg").and_then(Object::as_ref).map_or(here, |p| p == page_obj);
    let kids = kid_list(doc, &d);
    let before = kids.len();
    let keep: Vec<Object> = kids.into_iter().filter(|k| !mcr_mcid(doc, k, here, page_obj).is_some_and(|m| gone.contains(&m))).collect();
    let removed = before.saturating_sub(keep.len());
    if removed == 0 {
        return Ok((0, false));
    }
    let empty = keep.is_empty();
    doc.update_dict(e, |d| {
        if empty {
            d.remove(b"K");
        } else {
            d.set(b"K".to_vec(), Object::Array(keep));
        }
    })?;
    Ok((removed, empty))
}

/// Unlink elements left empty from their parents, and parents that become empty in turn (but
/// never a top-level element that wasn't emptied itself). `parent_of` gives an element's parent
/// (`None`: the root). Returns how many were removed.
fn prune(doc: &mut Document, root: ObjRef, emptied: Vec<ObjRef>, parent_of: &dyn Fn(ObjRef) -> Option<Option<ObjRef>>) -> Result<usize, TagError> {
    let mut n = 0usize;
    let mut todo = emptied;
    let mut done: HashSet<ObjRef> = HashSet::new();
    while let Some(e) = todo.pop() {
        if !done.insert(e) || done.len() > MAX_WALK {
            continue;
        }
        let Some(parent) = parent_of(e) else { continue };
        let holder = parent.unwrap_or(root);
        let Some(d) = doc.get(holder).as_dict().cloned() else { continue };
        let kids = kid_list(doc, &d);
        let keep: Vec<Object> = kids.iter().filter(|k| k.as_ref() != Some(e)).cloned().collect();
        if keep.len() == kids.len() {
            continue;
        }
        let empty = keep.is_empty();
        doc.update_dict(holder, |d| d.set(b"K".to_vec(), Object::Array(keep)))?;
        n = n.saturating_add(1);
        // A parent left empty goes too, unless it is top-level.
        if empty
            && let Some(p) = parent
            && parent_of(p).is_some_and(|pp| pp.is_some())
        {
            todo.push(p);
        }
    }
    Ok(n)
}

/// The parent tree (created when missing).
fn parent_tree(doc: &mut Document, root: ObjRef) -> Result<ObjRef, TagError> {
    let r = doc.get(root).as_dict().and_then(|d| d.get(b"ParentTree").cloned());
    match r {
        Some(Object::Ref(r)) if doc.get(r).as_dict().is_some() => Ok(r),
        Some(Object::Ref(_)) | None => {
            let pt = doc.add(Object::Dict({
                let mut d = Dict::new();
                d.set(b"Nums".to_vec(), Object::Array(Vec::new()));
                d
            }));
            doc.update_dict(root, |d| d.set(b"ParentTree".to_vec(), Object::Ref(pt)))?;
            Ok(pt)
        }
        Some(Object::Dict(d)) => {
            // Made indirect so it can be edited in place.
            let pt = doc.add(Object::Dict(d));
            doc.update_dict(root, |d| d.set(b"ParentTree".to_vec(), Object::Ref(pt)))?;
            Ok(pt)
        }
        Some(_) => Err(TagError::Invalid("the parent tree is not a dictionary; its entries were not updated".into())),
    }
}

/// Every node of a number tree (bounded, cycle-safe).
fn number_tree_nodes(doc: &Document, root: ObjRef) -> Vec<ObjRef> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    let mut seen = HashSet::new();
    while let Some(r) = stack.pop() {
        if !seen.insert(r) || seen.len() > MAX_TREE_NODES {
            continue;
        }
        let Some(d) = doc.get(r).as_dict().cloned() else { continue };
        out.push(r);
        if let Some(kids) = d.get(b"Kids").map(|o| doc.resolve(o)).and_then(|o| o.as_array().cloned()) {
            stack.extend(kids.iter().rev().filter_map(Object::as_ref));
        }
    }
    out
}

fn nums(doc: &Document, node: ObjRef) -> Option<Vec<Object>> {
    doc.get(node).as_dict().and_then(|d| d.get(b"Nums").cloned()).map(|o| doc.resolve(&o)).and_then(|o| o.as_array().cloned())
}

/// Set the parent-tree value for `key` (replacing it where it is, else adding it). Returns false
/// when the key isn't in the tree and can't be added in order (the caller then takes a new key).
fn put_number(doc: &mut Document, pt: ObjRef, key: i64, value: Object) -> Result<bool, TagError> {
    let nodes = number_tree_nodes(doc, pt);
    let mut max: Option<i64> = None;
    for &node in &nodes {
        let Some(mut list) = nums(doc, node) else { continue };
        let mut hit = false;
        for pair in list.chunks_mut(2) {
            let k = pair.first().and_then(Object::as_int);
            if let Some(k) = k {
                max = Some(max.map_or(k, |m| m.max(k)));
            }
            if k == Some(key)
                && let Some(v) = pair.get_mut(1)
            {
                *v = value.clone();
                hit = true;
            }
        }
        if hit {
            doc.update_dict(node, |d| d.set(b"Nums".to_vec(), Object::Array(list)))?;
            return Ok(true);
        }
    }
    let flat = doc.get(pt).as_dict().is_some_and(|d| !d.contains(b"Kids"));
    if flat {
        let mut list = nums(doc, pt).unwrap_or_default();
        let at = list.chunks(2).position(|c| c.first().and_then(Object::as_int).is_some_and(|k| k > key)).map_or(list.len(), |i| i.saturating_mul(2));
        list.insert(at.min(list.len()), value);
        list.insert(at.min(list.len()), Object::Int(key));
        doc.update_dict(pt, |d| d.set(b"Nums".to_vec(), Object::Array(list)))?;
        return Ok(true);
    }
    if max.is_some_and(|m| m >= key) {
        return Ok(false);
    }
    // Past every key: a new leaf at the end of the root's kids.
    let leaf = doc.add(Object::Dict({
        let mut d = Dict::new();
        d.set(b"Limits".to_vec(), Object::Array(vec![Object::Int(key), Object::Int(key)]));
        d.set(b"Nums".to_vec(), Object::Array(vec![Object::Int(key), value]));
        d
    }));
    let kids =
        doc.get(pt).as_dict().and_then(|d| d.get(b"Kids").cloned()).map(|k| doc.resolve(&k)).and_then(|k| k.as_array().cloned()).unwrap_or_default();
    let mut kids = kids;
    kids.push(Object::Ref(leaf));
    doc.update_dict(pt, |d| {
        d.set(b"Kids".to_vec(), Object::Array(kids));
        if let Some(Object::Array(l)) = d.get_mut(b"Limits")
            && let Some(hi) = l.get_mut(1)
        {
            *hi = Object::Int(key);
        }
    })?;
    Ok(true)
}

/// The largest key in the parent tree.
fn number_max(doc: &Document, pt: ObjRef) -> Option<i64> {
    number_tree_nodes(doc, pt)
        .into_iter()
        .filter_map(|n| nums(doc, n))
        .flat_map(|l| l.chunks(2).filter_map(|c| c.first().and_then(Object::as_int)).collect::<Vec<_>>())
        .max()
}

/// Rebuild page `page`'s parent-tree array (MCID → element) from the tree.
fn rebuild_parent_tree(doc: &mut Document, root: ObjRef, page: usize, page_obj: ObjRef) -> Result<(), TagError> {
    let tree = tags(doc);
    let mut slots: Vec<Object> = Vec::new();
    for n in &tree.nodes {
        let Some(obj) = n.obj else { continue };
        for k in &n.kids {
            if let TagKid::Content { page: Some(p), mcid, stream: None } = k
                && *p == page
                && let Ok(i) = usize::try_from(*mcid)
                && *mcid <= MAX_MCID
            {
                if slots.len() <= i {
                    slots.resize(i.saturating_add(1), Object::Null);
                }
                if let Some(s) = slots.get_mut(i) {
                    *s = Object::Ref(obj);
                }
            }
        }
    }
    let pt = parent_tree(doc, root)?;
    let key = doc.get(page_obj).as_dict().and_then(|d| d.get(b"StructParents").and_then(Object::as_int)).filter(|k| *k >= 0);
    if let Some(k) = key
        && put_number(doc, pt, k, Object::Array(slots.clone()))?
    {
        return Ok(());
    }
    if slots.is_empty() && key.is_none() {
        return Ok(());
    }
    // A new key past every key in use.
    let next = doc.get(root).as_dict().and_then(|d| d.get(b"ParentTreeNextKey").and_then(Object::as_int)).unwrap_or(0);
    let k = number_max(doc, pt).map_or(0, |m| m.saturating_add(1)).max(next).max(0);
    if !put_number(doc, pt, k, Object::Array(slots))? {
        return Err(TagError::Invalid("the parent tree has no room for this page's entry".into()));
    }
    doc.update_dict(root, |d| d.set(b"ParentTreeNextKey".to_vec(), Object::Int(k.saturating_add(1))))?;
    doc.update_dict(page_obj, |d| d.set(b"StructParents".to_vec(), Object::Int(k)))?;
    Ok(())
}

/// A BDC's operands without the text that spelled out its old content.
fn cleaned_operands(operands: &[Object]) -> (Vec<Object>, bool) {
    let mut out = operands.to_vec();
    let mut cleared = false;
    if let Some(Object::Dict(d)) = out.get_mut(1) {
        for key in [&b"ActualText"[..], b"Alt", b"E"] {
            cleared |= d.remove(key).is_some();
        }
    }
    (out, cleared)
}

fn op_bytes(op: &Op) -> Vec<u8> {
    let mut out = vec![b'\n'];
    pdfcraft_content::write_op(op, &mut out);
    out
}

/// Bring page `before.page()`'s tags in step with the edit made since `before` was taken.
/// Changes nothing (and reports nothing) when the page's marked content is as it was. On an
/// error the document is unchanged.
pub fn keep_tags(doc: &mut Document, before: &Snapshot) -> Result<TagUpdate, TagError> {
    let mut work = doc.clone();
    let report = keep_tags_inner(&mut work, before)?;
    if report.touched() || !report.warnings.is_empty() {
        *doc = work;
    }
    Ok(report)
}

fn keep_tags_inner(doc: &mut Document, before: &Snapshot) -> Result<TagUpdate, TagError> {
    let mut report = TagUpdate::default();
    let root = root_ref(doc)?;
    let Some(pg) = pdfcraft_model::pages(doc).into_iter().nth(before.page).filter(|p| p.obj == before.obj) else {
        report.warnings.push(format!("page {} is no longer where it was; its tags were not updated", before.page.saturating_add(1)));
        return Ok(report);
    };
    let streams = content_streams(doc, &pg.dict);
    let pieces = Pieces::join(&streams.iter().map(|(_, d)| d.as_slice()).collect::<Vec<_>>());
    let ops = pieces.parse().ops;
    if ops.len() > MAX_OPS {
        report.warnings.push(format!("page {} has too many operators to compare; its tags were not updated", before.page.saturating_add(1)));
        return Ok(report);
    }
    let after = scan(doc, &pg.dict, &ops);
    let (b_mc, a_mc) = (before.scan.by_mcid(), after.by_mcid());

    // Content that was untagged before is left as it was.
    let mut old_untagged: HashMap<&[u8], usize> = HashMap::new();
    for u in before.scan.units.iter().filter(|u| u.mcid.is_none() && !u.artifact) {
        let c = old_untagged.entry(u.sig.as_slice()).or_insert(0);
        *c = c.saturating_add(1);
    }
    let mut fresh: Vec<usize> = Vec::new();
    for (i, u) in after.units.iter().enumerate() {
        if u.mcid.is_some() || u.artifact {
            continue;
        }
        match old_untagged.get_mut(u.sig.as_slice()) {
            Some(c) if *c > 0 => *c -= 1,
            _ => fresh.push(i),
        }
    }

    // Marked content the edit emptied (in the order it was drawn), and content that changed.
    let mut emptied: Vec<(i64, Class)> = Vec::new();
    let mut changed: Vec<(i64, Class)> = Vec::new();
    for s in &before.scan.seqs {
        let Some(b) = b_mc.get(&s.mcid).filter(|b| b.drawing) else { continue };
        match a_mc.get(&s.mcid) {
            Some(a) if a.drawing => {
                if a.sig != b.sig {
                    changed.push((s.mcid, if a.text || b.text { Class::Text } else { b.class() }));
                }
            }
            _ => emptied.push((s.mcid, b.class())),
        }
    }
    if fresh.is_empty() && emptied.is_empty() && changed.is_empty() {
        return Ok(report);
    }

    // New units in runs: consecutive, same kind, same segment and content stream.
    let piece = |i: usize| ops.get(i).map(|o| (pieces.piece_of(o.span.start), pieces.piece_of(o.span.end.saturating_sub(1))));
    let mut runs: Vec<Run> = Vec::new();
    let mut last: Option<(usize, usize, usize)> = None; // (unit index, seg, piece)
    for &ui in &fresh {
        let Some(u) = after.units.get(ui) else { continue };
        let (Some((p0, p1)), Some((q0, q1))) = (piece(u.start), piece(u.end)) else { continue };
        if p0 != p1 || q0 != q1 || p0 != q0 {
            report.warnings.push(format!("content on page {} is split between content streams and was left untagged", before.page.saturating_add(1)));
            last = None;
            continue;
        }
        let joins = last.is_some_and(|(li, seg, lp)| li.saturating_add(1) == ui && seg == u.seg && lp == p0)
            && runs.last().is_some_and(|r| r.class == u.class);
        match runs.last_mut() {
            Some(r) if joins => r.end = u.end,
            _ => runs.push(Run { start: u.start, end: u.end, class: u.class }),
        }
        last = Some((ui, u.seg, p0));
    }

    // What becomes of each run.
    let mut used: HashSet<i64> = HashSet::new();
    let fates: Vec<Fate> = runs
        .iter()
        .map(|r| match emptied.iter().find(|(m, c)| *c == r.class && !used.contains(m)) {
            Some((m, _)) => {
                used.insert(*m);
                Fate::Adopt(*m)
            }
            None if r.class == Class::Path => Fate::Artifact,
            None => Fate::New(r.class),
        })
        .collect();
    let removed_mcids: HashSet<i64> = emptied.iter().map(|(m, _)| *m).filter(|m| !used.contains(m)).collect();

    let tree = tags(doc);
    let mut elements = Elements::new(&tree);
    // (page, MCID) → element.
    let owner: HashMap<i64, ObjRef> = tree
        .nodes
        .iter()
        .filter_map(|n| n.obj.map(|o| (o, n)))
        .flat_map(|(o, n)| {
            n.kids.iter().filter_map(move |k| match k {
                TagKid::Content { page: Some(p), mcid, stream: None } if *p == before.page => Some((*mcid, o)),
                _ => None,
            })
        })
        .collect();

    // Content stream edits: (start, end, order, bytes); insertions are empty ranges.
    let mut edits: Vec<(usize, usize, u8, Vec<u8>)> = Vec::new();
    let span = |i: usize| ops.get(i).map(|o| o.span.clone());
    let after_seq: HashMap<i64, &Seq> = after.seqs.iter().map(|s| (s.mcid, s)).collect();
    let before_seq: HashMap<i64, &Seq> = before.scan.seqs.iter().map(|s| (s.mcid, s)).collect();
    // Emptied sequences still on the page lose their (now empty) BDC/EMC pair.
    for (m, _) in &emptied {
        let Some(s) = after_seq.get(m) else { continue };
        let Some(emc) = s.emc else {
            report.warnings.push(format!("marked content {m} on page {} is not closed; it was left as it was", before.page.saturating_add(1)));
            continue;
        };
        if let (Some(a), Some(b)) = (span(s.bdc), span(emc)) {
            edits.push((a.start, a.end, 2, b"\n".to_vec()));
            edits.push((b.start, b.end, 2, b"\n".to_vec()));
        }
    }
    // Text that changed inside its sequence: the sequence's own ActualText goes.
    for (m, c) in &changed {
        if *c != Class::Text {
            continue;
        }
        let Some(s) = after_seq.get(m) else { continue };
        let (cleaned, cleared) = cleaned_operands(&s.operands);
        if cleared && let Some(a) = span(s.bdc) {
            edits.push((a.start, a.end, 2, op_bytes(&Op::new("BDC", cleaned))));
            report.actual_text_cleared = report.actual_text_cleared.saturating_add(1);
        }
    }

    let mut next_mcid = after.max_mcid.into_iter().chain(before.scan.max_mcid).max().map_or(0, |m| m.saturating_add(1));
    // The page's parent-tree array may be longer than the MCIDs in use.
    if let Some(len) = tree
        .nodes
        .iter()
        .flat_map(|n| n.kids.iter())
        .filter_map(|k| match k {
            TagKid::Content { page: Some(p), mcid, stream: None } if *p == before.page => Some(*mcid),
            _ => None,
        })
        .max()
    {
        next_mcid = next_mcid.max(len.saturating_add(1));
    }

    // Content order of tagged content after the edit, for placing new elements: (op, element).
    let mut placed: Vec<(usize, ObjRef)> =
        after.seqs.iter().filter(|s| !removed_mcids.contains(&s.mcid)).filter_map(|s| owner.get(&s.mcid).map(|o| (s.bdc, *o))).collect();
    let mut clear_actual: HashSet<ObjRef> = HashSet::new();
    for (run, fate) in runs.iter().zip(&fates) {
        let (Some(a), Some(b)) = (span(run.start), span(run.end)) else { continue };
        let (open, close) = match fate {
            Fate::Artifact => {
                report.artifacts = report.artifacts.saturating_add(1);
                (Op::new("BMC", vec![Object::name("Artifact")]), Op::new("EMC", vec![]))
            }
            Fate::Adopt(m) => {
                let operands = before_seq.get(m).map(|s| s.operands.clone()).unwrap_or_default();
                let (operands, cleared) = cleaned_operands(&operands);
                if cleared {
                    report.actual_text_cleared = report.actual_text_cleared.saturating_add(1);
                }
                report.adopted = report.adopted.saturating_add(1);
                if let Some(o) = owner.get(m) {
                    if run.class == Class::Text {
                        clear_actual.insert(*o);
                    } else if run.class == Class::Image && !report.review_alt.contains(o) {
                        report.review_alt.push(*o);
                    }
                }
                (Op::new("BDC", operands), Op::new("EMC", vec![]))
            }
            Fate::New(class) => {
                if !(0..MAX_MCID).contains(&next_mcid) {
                    report
                        .warnings
                        .push(format!("page {} has no free marked-content ids; new content was left untagged", before.page.saturating_add(1)));
                    continue;
                }
                let mcid = next_mcid;
                next_mcid = next_mcid.saturating_add(1);
                let role: &'static str = if *class == Class::Image { "Figure" } else { "P" };
                let element = place_element(doc, root, &tree, &mut elements, &placed, before.page, pg.obj, run.start, role, mcid)?;
                placed.push((run.start, element));
                report.added.push(AddedTag { page: before.page, element, role: role.into(), mcid });
                let mut props = Dict::new();
                props.set(b"MCID".to_vec(), Object::Int(mcid));
                (Op::new("BDC", vec![Object::name(role), Object::Dict(props)]), Op::new("EMC", vec![]))
            }
        };
        edits.push((a.start, a.start, 1, op_bytes(&open)));
        edits.push((b.end, b.end, 0, op_bytes(&close)));
    }

    // Changed content keeps its tag; text that changed loses the element's ActualText, and
    // figures are listed for review.
    for (m, c) in &changed {
        report.kept = report.kept.saturating_add(1);
        let Some(o) = owner.get(m) else { continue };
        match c {
            Class::Text => {
                clear_actual.insert(*o);
            }
            Class::Image => {
                if !report.review_alt.contains(o) {
                    report.review_alt.push(*o);
                }
            }
            Class::Path => {}
        }
    }
    for o in clear_actual {
        if doc.get(o).as_dict().is_some_and(|d| d.contains(b"ActualText")) {
            doc.update_dict(o, |d| {
                d.remove(b"ActualText");
            })?;
            report.actual_text_cleared = report.actual_text_cleared.saturating_add(1);
        }
    }
    // Figures keep their alternate text; only those that have some need a look.
    report.review_alt.retain(|o| doc.get(*o).as_dict().is_some_and(|d| d.contains(b"Alt")));

    // Content that went leaves its elements.
    if !removed_mcids.is_empty() {
        let mut emptied_elements = Vec::new();
        let holders: HashSet<ObjRef> = removed_mcids.iter().filter_map(|m| owner.get(m).copied()).collect();
        for e in holders {
            let here = elements.index.get(&e).and_then(|i| tree.nodes.get(*i)).and_then(|n| n.page) == Some(before.page);
            let (n, empty) = strip_mcids(doc, e, here, pg.obj, &removed_mcids)?;
            report.removed = report.removed.saturating_add(n);
            if empty {
                emptied_elements.push(e);
            }
        }
        let parent_of = |e: ObjRef| elements.parent(e);
        report.pruned = prune(doc, root, emptied_elements, &parent_of)?;
    }

    // Write the content.
    edits.sort_by_key(|(s, _, o, _)| (*s, *o));
    let mut at = 0usize;
    edits.retain(|(s, e, _, _)| {
        let ok = *s >= at;
        if ok {
            at = (*e).max(*s);
        }
        ok
    });
    if !edits.is_empty() {
        let new = pieces.splice(edits.into_iter().map(|(s, e, _, b)| (s..e, b)));
        let mut contents: Vec<Object> = Vec::with_capacity(streams.len());
        for ((obj, old), data) in streams.iter().zip(new) {
            if *old == data {
                contents.push(obj.clone());
                continue;
            }
            let mut dict = match &*doc.resolve(obj) {
                Object::Stream(s) => s.dict.clone(),
                _ => Dict::new(),
            };
            dict.remove(b"Length");
            contents.push(Object::Ref(doc.add(Object::Stream(Stream::flate(dict, &data)))));
        }
        let single = !matches!(pg.dict.get(b"Contents").map(|c| doc.resolve(c)).as_deref(), Some(Object::Array(_)));
        let value = match <[Object; 1]>::try_from(contents) {
            Ok([one]) if single => one,
            Ok([one]) => Object::Array(vec![one]),
            Err(many) => Object::Array(many),
        };
        doc.update_dict(pg.obj, |d| d.set(b"Contents".to_vec(), value))?;
    }

    if report.touched() {
        rebuild_parent_tree(doc, root, before.page, pg.obj)?;
        report.parent_tree = 1;
        report.pages.push(before.page);
    }
    Ok(report)
}

/// Make a `role` element for new content at operator `at` with `mcid`, next to the element of
/// the nearest tagged content (before it on the page, else after it, else on the nearest other
/// page), or at the end of the top-level grouping element.
#[allow(clippy::too_many_arguments)]
fn place_element(
    doc: &mut Document,
    root: ObjRef,
    tree: &Tags,
    elements: &mut Elements<'_>,
    placed: &[(usize, ObjRef)],
    page: usize,
    page_obj: ObjRef,
    at: usize,
    role: &'static str,
    mcid: i64,
) -> Result<ObjRef, TagError> {
    let before = placed.iter().filter(|(i, _)| *i < at).max_by_key(|(i, _)| *i).map(|(_, e)| *e);
    let after = placed.iter().filter(|(i, _)| *i > at).min_by_key(|(i, _)| *i).map(|(_, e)| *e);
    let on_page = |pred: &dyn Fn(usize) -> bool| -> Vec<ObjRef> {
        tree.nodes
            .iter()
            .filter(|n| n.kids.iter().any(|k| matches!(k, TagKid::Content { page: Some(p), .. } if pred(*p))))
            .filter_map(|n| n.obj)
            .collect()
    };
    let (anchor, after_anchor) = match (before, after) {
        (Some(e), _) => (Some(e), true),
        (None, Some(e)) => (Some(e), false),
        (None, None) => match on_page(&|p| p < page).last() {
            Some(e) => (Some(*e), true),
            None => (on_page(&|p| p > page).first().copied(), false),
        },
    };
    // Where it goes: beside the anchor's nearest ancestor that sits in a grouping element, or
    // inside the anchor when it is one.
    let (parent, sibling) = match anchor {
        Some(a) if elements.grouping(a) => (Some(a), None),
        Some(a) => {
            let mut e = a;
            let mut parent = elements.parent(e).unwrap_or(None);
            for _ in 0..MAX_CLIMB {
                match parent {
                    Some(p) if !elements.grouping(p) => {
                        e = p;
                        parent = elements.parent(p).unwrap_or(None);
                    }
                    _ => break,
                }
            }
            (parent, Some(e))
        }
        None => {
            // The single top-level element when it groups, else the root.
            let top: Vec<ObjRef> = tree.roots.iter().filter_map(|i| tree.nodes.get(*i)).filter_map(|n| n.obj).collect();
            match top.as_slice() {
                [one] if elements.grouping(*one) => (Some(*one), None),
                _ => (None, None),
            }
        }
    };
    let element = doc.add(Object::Dict({
        let mut d = Dict::new();
        d.set(b"Type".to_vec(), Object::name("StructElem"));
        d.set(b"S".to_vec(), Object::name(role));
        d.set(b"P".to_vec(), Object::Ref(parent.unwrap_or(root)));
        d.set(b"Pg".to_vec(), Object::Ref(page_obj));
        d.set(b"K".to_vec(), Object::Int(mcid));
        d
    }));
    insert_kid(doc, root, parent, sibling, anchor.is_none() || after_anchor, element)?;
    elements.made.insert(element, (parent, role));
    Ok(element)
}

/// The tags of pages about to be deleted (see [`drop_page_tags`]).
#[derive(Clone, Debug, Default)]
pub struct PagesSnapshot {
    /// (0-based page, page object).
    pages: Vec<(usize, ObjRef)>,
    /// Their parent-tree keys and their annotations'.
    keys: HashSet<i64>,
    annots: HashSet<ObjRef>,
}

/// The tags of `pages` (0-based) before they are deleted; `None` for an untagged document.
pub fn pages_snapshot(doc: &Document, pages: &[usize]) -> Option<PagesSnapshot> {
    let (_, cat) = catalog(doc)?;
    cat.get(b"StructTreeRoot")?;
    let all = pdfcraft_model::pages(doc);
    let mut snap = PagesSnapshot::default();
    for &p in pages {
        let Some(pg) = all.get(p) else { continue };
        snap.pages.push((p, pg.obj));
        if let Some(k) = pg.dict.get(b"StructParents").and_then(Object::as_int) {
            snap.keys.insert(k);
        }
        let annots = pg.dict.get(b"Annots").map(|a| doc.resolve(a)).and_then(|a| a.as_array().cloned()).unwrap_or_default();
        for a in annots.iter().take(100_000) {
            let Some(r) = a.as_ref() else { continue };
            snap.annots.insert(r);
            if let Some(k) = doc.get(r).as_dict().and_then(|d| d.get(b"StructParent").and_then(Object::as_int)) {
                snap.keys.insert(k);
            }
        }
    }
    Some(snap)
}

/// After pages were deleted: their marked content and annotations leave the tree, elements left
/// empty are removed, and their parent-tree entries go. On an error the document is unchanged.
pub fn drop_page_tags(doc: &mut Document, snap: &PagesSnapshot) -> Result<TagUpdate, TagError> {
    let mut work = doc.clone();
    let report = drop_page_tags_inner(&mut work, snap)?;
    if report.touched() || report.parent_tree > 0 {
        *doc = work;
    }
    Ok(report)
}

fn drop_page_tags_inner(doc: &mut Document, snap: &PagesSnapshot) -> Result<TagUpdate, TagError> {
    let mut report = TagUpdate::default();
    if snap.pages.is_empty() {
        return Ok(report);
    }
    let root = root_ref(doc)?;
    let gone: HashSet<ObjRef> = snap.pages.iter().map(|(_, r)| *r).collect();
    let root_dict = doc.get(root).as_dict().cloned().unwrap_or_default();
    // Raw walk: (kid, parent element or None for the root, inherited page).
    let mut stack: Vec<(Object, Option<ObjRef>, Option<ObjRef>)> = kid_list(doc, &root_dict).into_iter().rev().map(|k| (k, None, None)).collect();
    let mut seen: HashSet<ObjRef> = HashSet::new();
    let mut parents: HashMap<ObjRef, Option<ObjRef>> = HashMap::new();
    let mut emptied = Vec::new();
    while let Some((k, parent, pg)) = stack.pop() {
        let Object::Ref(r) = k else { continue };
        if !seen.insert(r) || seen.len() > MAX_WALK {
            continue;
        }
        let Some(d) = doc.get(r).as_dict().cloned() else { continue };
        if d.name(b"Type").is_some_and(|t| t == b"MCR" || t == b"OBJR") || !d.contains(b"S") {
            continue;
        }
        parents.insert(r, parent);
        let own = d.get(b"Pg").and_then(Object::as_ref);
        let here = own.or(pg);
        let on_gone = |p: Option<ObjRef>| p.is_some_and(|p| gone.contains(&p));
        let kids = kid_list(doc, &d);
        let mut keep = Vec::with_capacity(kids.len());
        let mut dropped = 0usize;
        for kid in kids {
            let resolved = match &kid {
                Object::Ref(x) => Some(doc.get(*x)),
                _ => None,
            };
            let as_dict = match (&kid, resolved.as_deref()) {
                (Object::Dict(d), _) => Some(d.clone()),
                (_, Some(Object::Dict(d))) => Some(d.clone()),
                _ => None,
            };
            let drop = match (&kid, &as_dict) {
                (Object::Int(_), _) => on_gone(here),
                (_, Some(x)) if x.name(b"Type") == Some(b"MCR") => on_gone(x.get(b"Pg").and_then(Object::as_ref).or(here)),
                (_, Some(x)) if x.name(b"Type") == Some(b"OBJR") => {
                    on_gone(x.get(b"Pg").and_then(Object::as_ref).or(here))
                        || x.get(b"Obj").and_then(Object::as_ref).is_some_and(|o| snap.annots.contains(&o))
                }
                _ => false,
            };
            if drop {
                dropped = dropped.saturating_add(1);
            } else {
                if let (Object::Ref(x), Some(x_d)) = (&kid, &as_dict)
                    && x_d.contains(b"S")
                {
                    stack.push((Object::Ref(*x), Some(r), here));
                }
                keep.push(kid);
            }
        }
        if dropped > 0 || on_gone(own) {
            report.removed = report.removed.saturating_add(dropped);
            let empty = keep.is_empty();
            let clear_pg = on_gone(own);
            doc.update_dict(r, |e| {
                if empty {
                    e.remove(b"K");
                } else {
                    e.set(b"K".to_vec(), Object::Array(keep));
                }
                if clear_pg {
                    e.remove(b"Pg");
                }
            })?;
            if empty && dropped > 0 {
                emptied.push(r);
            }
        }
    }
    let parent_of = |e: ObjRef| parents.get(&e).copied();
    report.pruned = prune(doc, root, emptied, &parent_of)?;

    // Their parent-tree entries go.
    if let Some(Object::Ref(pt)) = doc.get(root).as_dict().and_then(|d| d.get(b"ParentTree").cloned()) {
        for node in number_tree_nodes(doc, pt) {
            let Some(list) = nums(doc, node) else { continue };
            let mut keep = Vec::with_capacity(list.len());
            let mut cut = 0usize;
            for pair in list.chunks(2) {
                if pair.first().and_then(Object::as_int).is_some_and(|k| snap.keys.contains(&k)) {
                    cut = cut.saturating_add(1);
                } else {
                    keep.extend(pair.iter().cloned());
                }
            }
            if cut > 0 {
                report.parent_tree = report.parent_tree.saturating_add(cut);
                doc.update_dict(node, |d| d.set(b"Nums".to_vec(), Object::Array(keep)))?;
            }
        }
    }
    if report.touched() || report.parent_tree > 0 {
        report.pages = snap.pages.iter().map(|(p, _)| *p).collect();
    }
    Ok(report)
}
