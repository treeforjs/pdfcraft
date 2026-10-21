//! Linearized files, the viewer's "Fast Web View" (ISO 32000-1 and ISO 32000-2, Annex F).
//!
//! [`write_linearized`] writes the document so a viewer can show page 1 before the rest of the
//! file has arrived:
//!
//! 1. header; 2. the linearization parameter dictionary (`/Linearized 1 /L /H /O /E /N /T`);
//! 3. the first-page cross-reference section and trailer (`/Prev` → the main section);
//! 4. the catalog and the objects needed to open the document (`/ViewerPreferences`,
//!    `/OpenAction`, `/AcroForm`, `/Threads`, `/Encrypt`); 5. the primary hint stream (page
//!    offset and shared object hint tables, F.4); 6. page 1 and everything it uses; 7. every
//!    other page with its private objects, in page order; 8. objects shared by those pages;
//!    9. everything else (page tree nodes, outlines, names, metadata, `/Info`…);
//!    11. the main cross-reference section, whose `startxref` names the first-page section.
//!
//! Cross-reference sections are classic tables and nothing goes into object streams: hint
//! tables describe objects by byte position, which objects inside streams don't have, so
//! linearized output trades the compression of object streams for incremental loading (as
//! The viewer's own Fast Web View output on files without object streams). Encrypted documents
//! are linearized with their security kept: every object (the hint stream included) is
//! encrypted with its output number, and the `/Encrypt` dictionary is in part 4.
//!
//! Hint table offsets are, as F.4 requires, positions in the file as if the hint stream were not
//! present. Page groups have consecutive object numbers starting at the page object, so a
//! reader can find a page's objects from the page offset hint table alone. Content-stream
//! offsets and lengths (items 6–7 of the per-page entries) span the whole page, and every
//! shared object group holds one object.
//!
//! [`check_linearization`] reads a file's linearization back and checks it: that the parameter
//! dictionary is first, `/L` is the file length, `/O` the first page, `/N` the page count, `/E`
//! the end of the first-page section, `/T` the main cross-reference section, `/H` the hint
//! stream, and that the hint tables agree with where pages and shared objects really are.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::sync::Arc;

use crate::CosError;
use crate::document::{Document, ObjectReader, XrefEntry, read_xref_section};
use crate::object::{Dict, ObjRef, Object, Stream};
use crate::parser::Lexer;
use crate::writer::{SaveOptions, collect_refs, ensure_id, output_number, prepared, renumber, reread, stamp_mod_date, write_dict, write_indirect};

/// Page tree depth beyond which a tree is treated as malformed.
const MAX_TREE_DEPTH: usize = 256;
/// The most pages either direction handles (the viewer's practical limit is far lower).
const MAX_PAGES: usize = 1 << 21;
/// Catalog entries a viewer needs to open the document (part 4, F.3.5).
const OPEN_KEYS: [&[u8]; 4] = [b"ViewerPreferences", b"Threads", b"OpenAction", b"AcroForm"];
/// Page attributes inherited from page tree nodes (§7.7.3.4).
const INHERITABLE: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];
/// Dictionary keys a page's objects are not collected through: back links to parents and the
/// page itself, thumbnails and article beads (they belong to part 9).
const NOT_PAGE_CONTENT: [&[u8]; 4] = [b"Parent", b"P", b"Thumb", b"B"];

fn refuse(detail: impl Into<String>) -> CosError {
    CosError::Linearize(detail.into())
}

/// Where an object goes (the parts of F.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    /// Part 4: the catalog and document-level objects.
    Open,
    /// Part 6: page 1 and everything it uses.
    First,
    /// Part 7: private to one other page.
    Page(u32),
    /// Part 8: shared by pages other than page 1.
    Shared,
    /// Part 9: everything else.
    Other,
}

struct Node {
    r: ObjRef,
    is_stream: bool,
    /// Referenced objects, and whether collecting a page's objects follows the reference.
    edges: Vec<(u32, bool)>,
}

/// The objects reachable from the trailer, in breadth-first order.
struct Graph {
    nodes: Vec<Node>,
    index: HashMap<ObjRef, u32>,
}

/// References out of an object, and whether a page's objects are collected through each.
type Edges = Vec<(ObjRef, bool)>;

fn edges_of(o: &Object) -> Edges {
    let mut out = Vec::new();
    let dict = match o {
        Object::Dict(d) => Some(d),
        Object::Stream(s) => Some(&s.dict),
        _ => None,
    };
    match dict {
        Some(d) => {
            for (k, v) in d.iter() {
                let follow = !NOT_PAGE_CONTENT.contains(&k.as_slice());
                collect_refs(v, &mut |r| out.push((r, follow)));
            }
        }
        None => collect_refs(o, &mut |r| out.push((r, true))),
    }
    out
}

impl Graph {
    fn build(reader: &mut ObjectReader, trailer: &Dict) -> Result<Self, CosError> {
        let mut index: HashMap<ObjRef, u32> = HashMap::new();
        let mut found: Vec<(ObjRef, bool, Edges)> = Vec::new();
        let mut queue: std::collections::VecDeque<ObjRef> =
            [&b"Root"[..], b"Info", b"Encrypt"].into_iter().filter_map(|k| trailer.reference(k)).collect();
        while let Some(r) = queue.pop_front() {
            if index.contains_key(&r) {
                continue;
            }
            let o = reader.get(r);
            if matches!(*o, Object::Null) {
                continue; // dangling: written as null by renumbering
            }
            index.insert(r, output_number(found.len())?.saturating_sub(1));
            let edges = edges_of(&o);
            queue.extend(edges.iter().map(|(r, _)| *r));
            found.push((r, matches!(*o, Object::Stream(_)), edges));
        }
        let nodes = found
            .into_iter()
            .map(|(r, is_stream, edges)| Node {
                r,
                is_stream,
                edges: edges.into_iter().filter_map(|(c, f)| index.get(&c).map(|i| (*i, f))).collect(),
            })
            .collect();
        Ok(Self { nodes, index })
    }
}

/// The page tree's leaves in order, each with the references in the attributes it inherits,
/// and the intermediate nodes.
struct Tree {
    pages: Vec<(u32, Vec<u32>)>,
    nodes: HashSet<u32>,
}

/// Inheritable page attributes: (key, value).
type Attributes = Vec<(Vec<u8>, Object)>;
type Inherited = Arc<Attributes>;

/// Copy the attributes pages inherit from page tree nodes into the pages and drop them from the
/// nodes, so a page's objects in the hint tables are all a viewer needs to show it (qpdf and
/// The viewer's linearizers do the same). An inherited dictionary or array becomes one indirect
/// object the pages share; references and numbers are copied as they are. The document shows
/// the same pages afterwards.
fn push_inherited(doc: &mut Document, catalog: &Dict) -> Result<(), CosError> {
    let Some(root) = catalog.reference(b"Pages") else { return Ok(()) };
    let mut seen: HashSet<ObjRef> = HashSet::new();
    let mut nodes: Vec<ObjRef> = Vec::new();
    let mut pages: Vec<(ObjRef, Attributes)> = Vec::new();
    let mut stack: Vec<(ObjRef, usize, Inherited)> = vec![(root, 0, Arc::default())];
    while let Some((r, depth, inherited)) = stack.pop() {
        // Repeated or looping nodes are refused by `page_tree`; just don't walk them twice.
        if !seen.insert(r) || depth > MAX_TREE_DEPTH || seen.len() > MAX_PAGES {
            continue;
        }
        let o = doc.get(r);
        let Some(d) = o.as_dict() else { continue };
        let kids: Option<Vec<ObjRef>> = match d.get(b"Kids") {
            Some(Object::Array(a)) => Some(a.iter().filter_map(Object::as_ref).collect()),
            Some(Object::Ref(k)) => doc.get(*k).as_array().map(|a| a.iter().filter_map(Object::as_ref).collect()),
            _ => None,
        };
        if d.name(b"Type") == Some(b"Page") || (kids.is_none() && d.name(b"Type") != Some(b"Pages")) {
            let missing: Vec<(Vec<u8>, Object)> = inherited.iter().filter(|(k, _)| !d.contains(k)).cloned().collect();
            if !missing.is_empty() {
                pages.push((r, missing));
            }
            continue;
        }
        let mut next: Vec<(Vec<u8>, Object)> = inherited.iter().filter(|(k, _)| !d.contains(k)).cloned().collect();
        let mut own = false;
        for key in INHERITABLE {
            let Some(v) = d.get(key) else { continue };
            own = true;
            let v = match v {
                Object::Dict(_) | Object::Array(_) => Object::Ref(doc.add(v.clone())),
                other => other.clone(),
            };
            next.push((key.to_vec(), v));
        }
        if own {
            nodes.push(r);
        }
        let next = Arc::new(next);
        for kid in kids.unwrap_or_default().into_iter().rev() {
            stack.push((kid, depth + 1, next.clone()));
        }
    }
    for (page, missing) in pages {
        doc.update_dict(page, |d| {
            for (k, v) in missing {
                d.set(k, v);
            }
        })?;
    }
    for node in nodes {
        doc.update_dict(node, |d| {
            for key in INHERITABLE {
                d.remove(key);
            }
        })?;
    }
    Ok(())
}

fn page_tree(reader: &mut ObjectReader, graph: &Graph, catalog: &Dict) -> Result<Tree, CosError> {
    let root = catalog.reference(b"Pages").ok_or_else(|| refuse("the document has no page tree"))?;
    let mut tree = Tree { pages: Vec::new(), nodes: HashSet::new() };
    let mut seen: HashSet<ObjRef> = HashSet::new();
    let mut stack: Vec<(ObjRef, usize, Inherited)> = vec![(root, 0, Arc::default())];
    while let Some((r, depth, inherited)) = stack.pop() {
        if !seen.insert(r) {
            return Err(refuse(format!("object {} appears more than once in the page tree", r.num)));
        }
        if depth > MAX_TREE_DEPTH {
            return Err(refuse("the page tree is nested too deeply"));
        }
        let Some(&idx) = graph.index.get(&r) else { continue };
        let o = reader.get(r);
        let Some(d) = o.as_dict() else { continue };
        let kids: Option<Vec<ObjRef>> = match d.get(b"Kids") {
            Some(Object::Array(a)) => Some(a.iter().filter_map(Object::as_ref).collect()),
            Some(Object::Ref(k)) => reader.get(*k).as_array().map(|a| a.iter().filter_map(Object::as_ref).collect()),
            _ => None,
        };
        let leaf = d.name(b"Type") == Some(b"Page") || (kids.is_none() && d.name(b"Type") != Some(b"Pages"));
        if leaf {
            let mut refs = Vec::new();
            for (k, v) in inherited.iter() {
                if !d.contains(k) {
                    collect_refs(v, &mut |x| refs.extend(graph.index.get(&x).copied()));
                }
            }
            tree.pages.push((idx, refs));
            if tree.pages.len() > MAX_PAGES {
                return Err(refuse("the document has too many pages to linearize"));
            }
            continue;
        }
        tree.nodes.insert(idx);
        let mut next: Vec<(Vec<u8>, Object)> = inherited.iter().filter(|(k, _)| !d.contains(k)).cloned().collect();
        for key in INHERITABLE {
            if let Some(v) = d.get(key) {
                next.push((key.to_vec(), v.clone()));
            }
        }
        let next = Arc::new(next);
        for kid in kids.unwrap_or_default().into_iter().rev() {
            stack.push((kid, depth + 1, next.clone()));
        }
    }
    if tree.pages.is_empty() {
        return Err(refuse("the document has no pages"));
    }
    Ok(tree)
}

/// Where every object goes and in what order.
struct Layout {
    open: Vec<u32>,
    first: Vec<u32>,
    pages: Vec<Vec<u32>>,
    shared: Vec<u32>,
    other: Vec<u32>,
    /// For each page after the first: the objects it uses from parts 6 and 8.
    uses: Vec<Vec<u32>>,
    part: Vec<Part>,
    /// The outline hierarchy when it opens with page 1 (the end of part 6), root first.
    outlines: Vec<u32>,
}

fn layout(graph: &Graph, tree: &Tree, catalog: u32, trailer: &Dict, catalog_dict: &Dict) -> Layout {
    const NONE: u32 = u32::MAX;
    let n = graph.nodes.len();
    let mut page_of = vec![NONE; n];
    for (p, (idx, _)) in tree.pages.iter().enumerate() {
        if let Some(slot) = page_of.get_mut(*idx as usize) {
            *slot = p as u32;
        }
    }
    let blocked = |i: u32| page_of.get(i as usize).is_some_and(|p| *p != NONE) || tree.nodes.contains(&i);
    // Which pages use each object.
    let mut first = vec![false; n];
    let mut owner = vec![NONE; n];
    let mut multi = vec![false; n];
    let mut stamp = vec![0u32; n];
    let mut visits: Vec<Vec<u32>> = Vec::with_capacity(tree.pages.len());
    for (p, (idx, inherited)) in tree.pages.iter().enumerate() {
        let tag = p as u32 + 1;
        let mut stack: Vec<u32> = inherited.iter().rev().copied().collect();
        if let Some(node) = graph.nodes.get(*idx as usize) {
            stack.extend(node.edges.iter().rev().filter(|(_, f)| *f).map(|(c, _)| *c));
        }
        let mut list = Vec::new();
        while let Some(i) = stack.pop() {
            let Some(s) = stamp.get_mut(i as usize) else { continue };
            if *s == tag || blocked(i) {
                continue;
            }
            *s = tag;
            list.push(i);
            let i = i as usize;
            if p == 0 {
                first[i] = true;
            } else if owner[i] == NONE {
                owner[i] = p as u32;
            } else if owner[i] != p as u32 {
                multi[i] = true;
            }
            if let Some(node) = graph.nodes.get(i) {
                stack.extend(node.edges.iter().rev().filter(|(_, f)| *f).map(|(c, _)| *c));
            }
        }
        visits.push(list);
    }
    // What opening the document needs.
    let mut open = vec![false; n];
    let mut stack: Vec<u32> = Vec::new();
    let seed = |o: &Object, stack: &mut Vec<u32>| collect_refs(o, &mut |r| stack.extend(graph.index.get(&r).copied()));
    for key in OPEN_KEYS {
        if let Some(v) = catalog_dict.get(key) {
            seed(v, &mut stack);
        }
    }
    if let Some(v) = trailer.get(b"Encrypt") {
        seed(v, &mut stack);
    }
    while let Some(i) = stack.pop() {
        if i == catalog || blocked(i) {
            continue;
        }
        let Some(flag) = open.get_mut(i as usize) else { continue };
        if *flag {
            continue;
        }
        *flag = true;
        if let Some(node) = graph.nodes.get(i as usize) {
            stack.extend(node.edges.iter().map(|(c, _)| *c));
        }
    }
    let mut part: Vec<Part> = (0..n)
        .map(|i| {
            if i == catalog as usize || open[i] {
                Part::Open
            } else if page_of[i] == 0 || (page_of[i] == NONE && first[i]) {
                Part::First
            } else if page_of[i] != NONE {
                Part::Page(page_of[i])
            } else if multi[i] {
                Part::Shared
            } else if owner[i] != NONE {
                Part::Page(owner[i])
            } else {
                Part::Other
            }
        })
        .collect();
    // With /PageMode /UseOutlines the outlines are shown with page 1: they end part 6 (F.3.6),
    // and the outline hint table describes them.
    let mut outlines = Vec::new();
    if catalog_dict.name(b"PageMode") == Some(b"UseOutlines")
        && let Some(root) = catalog_dict.reference(b"Outlines").and_then(|r| graph.index.get(&r).copied())
    {
        let mut stack = vec![root];
        while let Some(i) = stack.pop() {
            if blocked(i) {
                continue;
            }
            let Some(p) = part.get_mut(i as usize) else { continue };
            if matches!(*p, Part::Open | Part::First) {
                continue;
            }
            *p = Part::First;
            outlines.push(i);
            if let Some(node) = graph.nodes.get(i as usize) {
                stack.extend(node.edges.iter().rev().map(|(c, _)| *c));
            }
        }
    }
    let mut out = Layout {
        open: vec![catalog],
        first: Vec::new(),
        pages: Vec::new(),
        shared: Vec::new(),
        other: Vec::new(),
        uses: Vec::new(),
        part,
        outlines: Vec::new(),
    };
    out.open.extend((0..n as u32).filter(|i| *i != catalog && out.part[*i as usize] == Part::Open));
    for (p, ((idx, _), list)) in tree.pages.iter().zip(&visits).enumerate() {
        let want = if p == 0 { Part::First } else { Part::Page(p as u32) };
        let mut group = vec![*idx];
        group.extend(list.iter().copied().filter(|i| out.part.get(*i as usize) == Some(&want)));
        if p == 0 {
            group.extend(&outlines);
            out.first = group;
        } else {
            out.pages.push(group);
        }
    }
    let mut placed = vec![false; n];
    for list in visits.iter().skip(1) {
        for &i in list {
            if out.part.get(i as usize) == Some(&Part::Shared) && !placed[i as usize] {
                placed[i as usize] = true;
                out.shared.push(i);
            }
        }
        out.uses.push(list.iter().copied().filter(|i| matches!(out.part.get(*i as usize), Some(Part::First | Part::Shared))).collect());
    }
    out.other.extend((0..n as u32).filter(|i| out.part[*i as usize] == Part::Other));
    out.outlines = outlines;
    out
}

/// Big-endian bit packing for the hint tables (F.4).
#[derive(Default)]
struct BitWriter {
    out: Vec<u8>,
    acc: u8,
    used: u32,
}

impl BitWriter {
    fn put(&mut self, value: u64, bits: u32) {
        for i in (0..bits.min(64)).rev() {
            self.acc = (self.acc << 1) | ((value >> i) & 1) as u8;
            self.used += 1;
            if self.used == 8 {
                self.out.push(self.acc);
                self.acc = 0;
                self.used = 0;
            }
        }
    }

    fn align(&mut self) {
        if self.used > 0 {
            self.put(0, 8 - self.used);
        }
    }
}

/// Bits needed to hold `v` (0 for 0).
fn bits_for(v: u64) -> u32 {
    64 - v.leading_zeros()
}

fn hint_value(v: u64) -> Result<u64, CosError> {
    u32::try_from(v).map(u64::from).map_err(|_| refuse("the file is larger than linearization hint tables can describe (4 GB)"))
}

struct Placed {
    offset: Vec<u64>,
    length: Vec<u64>,
}

impl Placed {
    fn len_of(&self, group: &[u32]) -> u64 {
        group.iter().map(|i| self.length.get(*i as usize).copied().unwrap_or(0)).fold(0u64, u64::saturating_add)
    }
    fn at(&self, i: u32) -> u64 {
        self.offset.get(i as usize).copied().unwrap_or(0)
    }
}

/// The primary hint stream's data: the page offset hint table, then the shared object hint
/// table, which starts at the returned byte offset (`/S`), then the outline hint table when the
/// outlines open with page 1 (`/O`). Offsets are without the hint stream.
fn hint_tables(l: &Layout, placed: &Placed, number: &[u32]) -> Result<(Vec<u8>, usize, Option<usize>), CosError> {
    // Shared object identifiers: page 1's objects first, then part 8 (F.4.2).
    let mut shared_id: HashMap<u32, u64> = HashMap::new();
    for (k, i) in l.first.iter().chain(&l.shared).enumerate() {
        shared_id.insert(*i, k as u64);
    }
    struct PageRow {
        objects: u64,
        length: u64,
        shared: Vec<u64>,
    }
    let mut rows = vec![PageRow { objects: l.first.len() as u64, length: hint_value(placed.len_of(&l.first))?, shared: Vec::new() }];
    for (group, uses) in l.pages.iter().zip(&l.uses) {
        rows.push(PageRow {
            objects: group.len() as u64,
            length: hint_value(placed.len_of(group))?,
            shared: uses.iter().filter_map(|i| shared_id.get(i).copied()).collect(),
        });
    }
    let min_objects = rows.iter().map(|r| r.objects).min().unwrap_or(0);
    let max_objects = rows.iter().map(|r| r.objects).max().unwrap_or(0);
    let min_length = rows.iter().map(|r| r.length).min().unwrap_or(0);
    let max_length = rows.iter().map(|r| r.length).max().unwrap_or(0);
    let max_refs = rows.iter().map(|r| r.shared.len() as u64).max().unwrap_or(0);
    let max_id = rows.iter().flat_map(|r| r.shared.iter().copied()).max().unwrap_or(0);
    let (objects_bits, length_bits) = (bits_for(max_objects - min_objects), bits_for(max_length - min_length));
    let (refs_bits, id_bits) = (bits_for(max_refs), bits_for(max_id));
    let first_page = hint_value(l.first.first().map_or(0, |i| placed.at(*i)))?;
    let mut w = BitWriter::default();
    // Table F.3: the page offset hint table header.
    w.put(hint_value(min_objects)?, 32);
    w.put(first_page, 32);
    w.put(u64::from(objects_bits), 16);
    w.put(min_length, 32);
    w.put(u64::from(length_bits), 16);
    w.put(0, 32); // least content stream offset (contents span the page)
    w.put(0, 16);
    w.put(min_length, 32); // least content stream length
    w.put(u64::from(length_bits), 16);
    w.put(u64::from(refs_bits), 16);
    w.put(u64::from(id_bits), 16);
    w.put(0, 16); // bits for the numerator of a shared reference's position: none
    w.put(1, 16); // its denominator
    // Table F.4: the per-page entries, item by item, each item starting on a byte.
    for r in &rows {
        w.put(r.objects - min_objects, objects_bits);
    }
    w.align();
    for r in &rows {
        w.put(r.length - min_length, length_bits);
    }
    w.align();
    for r in &rows {
        w.put(r.shared.len() as u64, refs_bits);
    }
    w.align();
    for r in &rows {
        for id in &r.shared {
            w.put(*id, id_bits);
        }
    }
    w.align();
    // Numerators take no bits; content offsets are all 0.
    for r in &rows {
        w.put(r.length - min_length, length_bits);
    }
    w.align();
    let shared_at = w.out.len();
    // Table F.5: the shared object hint table header.
    let groups: Vec<u64> =
        l.first.iter().chain(&l.shared).map(|i| hint_value(placed.length.get(*i as usize).copied().unwrap_or(0))).collect::<Result<_, _>>()?;
    let min_group = groups.iter().copied().min().unwrap_or(0);
    let group_bits = bits_for(groups.iter().copied().max().unwrap_or(0) - min_group);
    let first_shared = l.shared.first().map_or(0, |i| number.get(*i as usize).copied().unwrap_or(0));
    w.put(u64::from(first_shared), 32);
    w.put(hint_value(l.shared.first().map_or(0, |i| placed.at(*i)))?, 32);
    w.put(hint_value(l.first.len() as u64)?, 32);
    w.put(hint_value(groups.len() as u64)?, 32);
    w.put(0, 16); // one object per group
    w.put(min_group, 32);
    w.put(u64::from(group_bits), 16);
    // Table F.6: group lengths, then signature flags (none); objects per group take no bits.
    for g in &groups {
        w.put(g - min_group, group_bits);
    }
    w.align();
    for _ in &groups {
        w.put(0, 1);
    }
    w.align();
    // Table F.11: a generic hint table for the outlines.
    let outline_at = match l.outlines.first() {
        Some(root) => {
            let at = w.out.len();
            w.put(u64::from(number.get(*root as usize).copied().unwrap_or(0)), 32);
            w.put(hint_value(placed.at(*root))?, 32);
            w.put(hint_value(l.outlines.len() as u64)?, 32);
            w.put(hint_value(placed.len_of(&l.outlines))?, 32);
            Some(at)
        }
        None => None,
    };
    Ok((w.out, shared_at, outline_at))
}

struct LinParams {
    l: u64,
    h: (u64, u64),
    o: u32,
    e: u64,
    n: usize,
    t: u64,
}

fn lin_dict(p: &LinParams) -> Vec<u8> {
    format!("<</Linearized 1/L {}/H[{} {}]/O {}/E {}/N {}/T {}>>", p.l, p.h.0, p.h.1, p.o, p.e, p.n, p.t).into_bytes()
}

/// `bytes` followed by spaces up to `width` (the space reserved for it).
fn padded(mut bytes: Vec<u8>, width: usize) -> Result<Vec<u8>, CosError> {
    if bytes.len() > width {
        return Err(refuse("internal layout error: a reserved field overflowed"));
    }
    bytes.resize(width, b' ');
    Ok(bytes)
}

/// Write the document linearized ("Fast Web View", Annex F): a full rewrite of the objects
/// reachable from the trailer, renumbered and ordered so page 1 can be shown first.
///
/// Fails with [`CosError::Linearize`] for a document without pages, or whose page tree repeats
/// a page or loops.
pub fn write_linearized(doc: &Document, opts: &SaveOptions) -> Result<Vec<u8>, CosError> {
    if doc.stream_limited() {
        return Err(CosError::ReadOnlyLimit);
    }
    let mut reader = doc.object_reader();
    stamp_mod_date(&mut reader.document, opts);
    let trailer_in = reader.document.trailer().clone();
    let root = trailer_in.reference(b"Root").ok_or_else(|| refuse("the document has no catalog"))?;
    let catalog_dict = reader.get(root).as_dict().cloned().ok_or_else(|| refuse("the catalog is not a dictionary"))?;
    push_inherited(&mut reader.document, &catalog_dict)?;
    let graph = Graph::build(&mut reader, &trailer_in)?;
    let catalog = *graph.index.get(&root).ok_or_else(|| refuse("the document has no catalog"))?;
    let tree = page_tree(&mut reader, &graph, &catalog_dict)?;
    let l = layout(&graph, &tree, catalog, &trailer_in, &catalog_dict);

    // Numbers: the main section (parts 7–9) from 1, then the first-page section: the
    // parameter dictionary, part 4, part 6 and the hint stream.
    let n = graph.nodes.len();
    let mut number = vec![0u32; n];
    let main: Vec<u32> = l.pages.iter().flatten().chain(&l.shared).chain(&l.other).copied().collect();
    for (k, i) in main.iter().enumerate() {
        number[*i as usize] = output_number(k)?;
    }
    let main_count = output_number(main.len())?; // entries 0..main_count
    let lin_num = main_count;
    let mut next = lin_num;
    for i in l.open.iter().chain(&l.first) {
        next = next.checked_add(1).ok_or_else(|| refuse("too many objects"))?;
        number[*i as usize] = next;
    }
    if number.contains(&0) {
        return Err(refuse("internal layout error: an object was not placed"));
    }
    let hint_num = next.checked_add(1).ok_or_else(|| refuse("too many objects"))?;
    let size = hint_num.checked_add(1).ok_or_else(|| refuse("too many objects"))?;
    let map: HashMap<ObjRef, u32> = graph.nodes.iter().zip(&number).map(|(node, num)| (node.r, *num)).collect();

    let mut trailer = Dict::new();
    for key in [&b"Root"[..], b"Info", b"Encrypt"] {
        if let Some(r) = trailer_in.reference(key).and_then(|r| map.get(&r)) {
            trailer.set(key.to_vec(), Object::Ref(ObjRef::new(*r, 0)));
        }
    }
    if let Some(Object::Dict(d)) = trailer_in.get(b"Encrypt") {
        trailer.set(b"Encrypt".to_vec(), Object::Dict(d.clone()));
    }
    if let Some(id) = trailer_in.get(b"ID") {
        trailer.set(b"ID".to_vec(), id.clone());
    }
    if reader.document.output_security().0.is_none() {
        ensure_id(&mut trailer, opts, reader.document.bytes());
    }
    let first_trailer = |prev: u64| {
        let mut t = trailer.clone();
        t.set(b"Size".to_vec(), Object::Int(i64::from(size)));
        t.set(b"Prev".to_vec(), Object::Int(i64::try_from(prev).unwrap_or(i64::MAX)));
        let mut out = Vec::new();
        write_dict(&t, &mut out);
        out
    };

    // Linearization needs PDF 1.2.
    let version = reader.document.version().chars().take(3).collect::<String>();
    let version = if version.as_str() < "1.2" { "1.2".to_string() } else { version };
    let mut out = Vec::new();
    let _ = writeln!(out, "%PDF-{version}");
    out.extend_from_slice(&[b'%', 0xE2, 0xE3, 0xCF, 0xD3, b'\n']);
    // Parts 2 and 3 are written last, into space reserved at their final size.
    let lin_at = out.len();
    let wide = u64::MAX;
    let first_page_num = l.first.first().map_or(0, |i| number[*i as usize]);
    let lin_width = lin_dict(&LinParams { l: wide, h: (wide, wide), o: first_page_num, e: wide, n: tree.pages.len(), t: wide }).len();
    let lin_head = format!("{lin_num} 0 obj\n");
    let fx_at = lin_at + lin_head.len() + lin_width + b"\nendobj\n".len();
    let first_count = (size - lin_num) as usize;
    let fx_head = format!("xref\n{lin_num} {first_count}\n");
    let trailer_width = first_trailer(wide).len();
    let fx_tail = b"\nstartxref\n0\n%%EOF\n";
    let fx_len = fx_head.len() + 20 * first_count + b"trailer\n".len() + trailer_width + fx_tail.len();
    out.resize(fx_at + fx_len, b' ');

    let mut placed = Placed { offset: vec![0; n], length: vec![0; n] };
    let mut emit = |i: u32, out: &mut Vec<u8>, reader: &mut ObjectReader| -> Result<(), CosError> {
        let Some(node) = graph.nodes.get(i as usize) else { return Ok(()) };
        let num = number[i as usize];
        let start = out.len();
        let o = renumber(&*reread(reader, node.r, node.is_stream)?, &map);
        write_indirect(num, 0, &prepared(&reader.document, node.r.num, num, 0, &o), out);
        placed.offset[i as usize] = start as u64;
        placed.length[i as usize] = (out.len() - start) as u64;
        Ok(())
    };
    for &i in &l.open {
        emit(i, &mut out, &mut reader)?;
    }
    let hint_at = out.len();
    for &i in l.first.iter().chain(&main) {
        emit(i, &mut out, &mut reader)?;
    }
    // Part 5, computed with the offsets as they are now: without the hint stream.
    let (data, shared_at, outline_at) = hint_tables(&l, &placed, &number)?;
    let mut hd = Dict::new();
    hd.set(b"S".to_vec(), Object::Int(shared_at as i64));
    if let Some(at) = outline_at {
        hd.set(b"O".to_vec(), Object::Int(at as i64));
    }
    let hint = Object::Stream(Stream::flate(hd, &data));
    let mut hint_bytes = Vec::new();
    write_indirect(hint_num, 0, &prepared(&reader.document, u32::MAX, hint_num, 0, &hint), &mut hint_bytes);
    let hint_len = hint_bytes.len() as u64;
    out.splice(hint_at..hint_at, hint_bytes);
    for (offset, part) in placed.offset.iter_mut().zip(&l.part) {
        if *part != Part::Open {
            *offset += hint_len;
        }
    }

    // Part 11: the main cross-reference section.
    let main_at = out.len();
    let _ = write!(out, "xref\n0 {main_count}");
    let t = out.len() as u64; // the end of line before the entry for object 0
    out.extend_from_slice(b"\n0000000000 65535 f \n");
    for &i in &main {
        let _ = writeln!(out, "{:010} 00000 n ", placed.at(i));
    }
    let mut main_trailer = trailer.clone();
    main_trailer.set(b"Size".to_vec(), Object::Int(i64::from(main_count)));
    out.extend_from_slice(b"trailer\n");
    write_dict(&main_trailer, &mut out);
    let _ = write!(out, "\nstartxref\n{fx_at}\n%%EOF\n");

    // Parts 2 and 3, now that every position is known.
    let end_of_first = l.first.last().map_or(0, |i| placed.at(*i).saturating_add(placed.length.get(*i as usize).copied().unwrap_or(0)));
    let params = LinParams { l: out.len() as u64, h: (hint_at as u64, hint_len), o: first_page_num, e: end_of_first, n: tree.pages.len(), t };
    let mut head = lin_head.into_bytes();
    head.extend_from_slice(&padded(lin_dict(&params), lin_width)?);
    head.extend_from_slice(b"\nendobj\n");
    head.extend_from_slice(fx_head.as_bytes());
    let _ = writeln!(head, "{:010} 00000 n ", lin_at);
    for &i in l.open.iter().chain(&l.first) {
        let _ = writeln!(head, "{:010} 00000 n ", placed.at(i));
    }
    let _ = writeln!(head, "{:010} 00000 n ", hint_at);
    head.extend_from_slice(b"trailer\n");
    head.extend_from_slice(&padded(first_trailer(main_at as u64), trailer_width)?);
    head.extend_from_slice(fx_tail);
    let slot = out.get_mut(lin_at..fx_at + fx_len).ok_or_else(|| refuse("internal layout error"))?;
    if slot.len() != head.len() {
        return Err(refuse("internal layout error: the first-page section changed size"));
    }
    slot.copy_from_slice(&head);
    out.shrink_to_fit();
    Ok(out)
}

// ── checking ─────────────────────────────────────────────────────────────────────────────────

/// The linearization parameter dictionary's values as found (`None` when missing or not an
/// integer of the right kind).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LinearizationParams {
    /// `/Linearized`: the version of linearization (1).
    pub version: Option<f64>,
    /// `/L`: the file's length in bytes.
    pub file_length: Option<i64>,
    /// `/H`: the primary hint stream's offset and length.
    pub hint_offset: Option<i64>,
    pub hint_length: Option<i64>,
    /// `/H` entries 3–4, when present: an overflow hint stream.
    pub overflow_hint: Option<(i64, i64)>,
    /// `/O`: the first page's object number.
    pub first_page_object: Option<i64>,
    /// `/E`: the offset of the end of the first page's section.
    pub first_page_end: Option<i64>,
    /// `/N`: the number of pages.
    pub page_count: Option<i64>,
    /// `/T`: where the main cross-reference section's first entry starts.
    pub main_xref_first_entry: Option<i64>,
}

/// What [`check_linearization`] found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LinearizationReport {
    /// The file starts with a linearization parameter dictionary.
    pub linearized: bool,
    /// Linearized, and every check passed: a viewer can rely on the linearization.
    pub valid: bool,
    /// Why the linearization can't be relied on (empty when valid).
    pub problems: Vec<String>,
    /// Checks that could not be made (for example an encrypted hint stream read without its
    /// password); they don't make the file invalid.
    pub notes: Vec<String>,
    pub params: Option<LinearizationParams>,
    /// The file's actual length.
    pub file_length: usize,
    /// Pages found in the page tree.
    pub pages_found: Option<usize>,
}

/// At most this many problems are listed.
const MAX_PROBLEMS: usize = 40;
/// Shared object references a page offset hint table may list in total.
const MAX_SHARED_REFS: u64 = 1 << 22;

/// Whether the file is linearized with a length that matches (what the viewer's Document
/// Properties shows as "Fast Web View: Yes"). Cheap: reads only the first object.
pub fn fast_web_view(bytes: &[u8]) -> bool {
    matches!(first_object(bytes), Some((_, d, _)) if d.get(b"Linearized").is_some() && d.int(b"L").is_some_and(|l| usize::try_from(l).ok() == Some(bytes.len())))
}

/// The first indirect object in the first 1024 bytes after the header, when it's a dictionary:
/// (its offset, the dictionary, the offset after `endobj`).
fn first_object(data: &[u8]) -> Option<(usize, Dict, usize)> {
    let head = data.get(..data.len().min(1024))?;
    let header = head.windows(5).position(|w| w == b"%PDF-")?;
    let mut lx = Lexer::new(data, header);
    lx.skip_ws(); // the header line and binary comment are comments
    let at = lx.pos;
    if at >= header.saturating_add(1024) {
        return None;
    }
    let (_, o, end) = indirect_at(data, at)?;
    Some((at, o.as_dict()?.clone(), end))
}

/// A non-stream indirect object at `at`: (reference, object, offset after `endobj`).
fn indirect_at(data: &[u8], at: usize) -> Option<(ObjRef, Object, usize)> {
    let mut lx = Lexer::new(data, at);
    lx.skip_ws();
    let num = std::str::from_utf8(lx.token()).ok()?.parse::<u32>().ok()?;
    lx.skip_ws();
    let generation = std::str::from_utf8(lx.token()).ok()?.parse::<u16>().ok()?;
    if !lx.eat_keyword(b"obj") {
        return None;
    }
    let o = lx.object().ok()?;
    if !lx.eat_keyword(b"endobj") {
        return None;
    }
    Some((ObjRef::new(num, generation), o, lx.pos))
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl BitReader<'_> {
    fn get(&mut self, bits: u32) -> Option<u64> {
        if bits > 64 {
            return None;
        }
        let mut v = 0u64;
        for _ in 0..bits {
            let byte = *self.data.get(self.pos / 8)?;
            v = (v << 1) | u64::from((byte >> (7 - self.pos % 8)) & 1);
            self.pos += 1;
        }
        Some(v)
    }

    fn align(&mut self) {
        self.pos = self.pos.div_ceil(8).saturating_mul(8);
    }
}

struct Checker<'a> {
    data: &'a [u8],
    problems: Vec<String>,
    notes: Vec<String>,
    entries: BTreeMap<u32, XrefEntry>,
    boundaries: Vec<usize>,
    reader: Option<ObjectReader>,
    /// Object lookups the hint table checks may still make (hostile tables could otherwise ask
    /// for the same long runs of objects over and over).
    budget: u64,
}

impl Checker<'_> {
    fn problem(&mut self, p: impl Into<String>) {
        if self.problems.len() < MAX_PROBLEMS {
            self.problems.push(p.into());
        } else if self.problems.len() == MAX_PROBLEMS {
            self.problems.push("…and more problems".into());
        }
    }

    fn note(&mut self, n: impl Into<String>) {
        let n = n.into();
        if !self.notes.contains(&n) && self.notes.len() < MAX_PROBLEMS {
            self.notes.push(n);
        }
    }

    fn offset(&self, num: u32) -> Option<usize> {
        match self.entries.get(&num) {
            Some(XrefEntry::InFile { offset, .. }) => usize::try_from(*offset).ok(),
            _ => None,
        }
    }

    /// The first boundary (object or section start, or the end of the file) after `off`.
    fn next_boundary(&self, off: usize) -> usize {
        let k = self.boundaries.partition_point(|b| *b <= off);
        self.boundaries.get(k).copied().unwrap_or(self.data.len())
    }

    /// Where the object starting at `off` ends: just after `endobj`, and the next boundary.
    fn extent(&self, off: usize) -> Option<(usize, usize)> {
        let next = self.next_boundary(off);
        let w = self.data.get(off..next)?;
        let k = w.windows(6).rposition(|x| x == b"endobj")?;
        Some((off + k + 6, next))
    }

    /// The bytes taken by the `n` objects numbered from `first` on, laid out one after another
    /// (each up to the next object or section); `None` when one isn't a plain object in the file.
    fn length_of_run(&mut self, first: u32, n: u64) -> Option<u64> {
        let mut total = 0u64;
        if n > self.budget {
            self.budget = 0;
            self.note("the hint tables ask for more objects than a check reads; the rest were not checked");
            return None;
        }
        self.budget -= n;
        for k in 0..n {
            let num = first.checked_add(u32::try_from(k).ok()?)?;
            let off = self.offset(num)?;
            total = total.checked_add((self.next_boundary(off) - off) as u64)?;
        }
        Some(total)
    }

    fn get(&mut self, r: ObjRef) -> Option<Arc<Object>> {
        if let Some(reader) = self.reader.as_mut() {
            return reader.try_get(r).ok().filter(|o| !matches!(**o, Object::Null));
        }
        let off = self.offset(r.num)?;
        let entries = &self.entries;
        let data = self.data;
        let length = |x: ObjRef| match entries.get(&x.num) {
            Some(XrefEntry::InFile { offset, .. }) => indirect_at(data, usize::try_from(*offset).ok()?).and_then(|(_, o, _)| o.as_int()),
            _ => None,
        };
        match crate::parse_indirect(data, off, &length) {
            Ok((id, o)) if id.num == r.num => Some(Arc::new(o)),
            _ => None,
        }
    }

    fn pages(&mut self, catalog: ObjRef) -> Option<Vec<ObjRef>> {
        let cat = self.get(catalog)?;
        let root = cat.as_dict()?.reference(b"Pages")?;
        let mut pages = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = vec![(root, 0usize)];
        while let Some((r, depth)) = stack.pop() {
            if !seen.insert(r) || depth > MAX_TREE_DEPTH || pages.len() > MAX_PAGES {
                self.problem("the page tree loops or is malformed");
                return Some(pages);
            }
            let Some(o) = self.get(r) else { continue };
            let Some(d) = o.as_dict() else { continue };
            let kids: Option<Vec<ObjRef>> = match d.get(b"Kids") {
                Some(Object::Array(a)) => Some(a.iter().filter_map(Object::as_ref).collect()),
                Some(Object::Ref(k)) => {
                    let k = *k;
                    self.get(k).and_then(|a| a.as_array().map(|a| a.iter().filter_map(Object::as_ref).collect()))
                }
                _ => None,
            };
            if d.name(b"Type") == Some(b"Page") || (kids.is_none() && d.name(b"Type") != Some(b"Pages")) {
                pages.push(r);
            } else {
                stack.extend(kids.unwrap_or_default().into_iter().rev().map(|k| (k, depth + 1)));
            }
        }
        Some(pages)
    }
}

/// Check whether `bytes` is a linearized file and whether its linearization is consistent.
///
/// `doc`, when given, is the same file opened (with its password, for an encrypted file): its
/// objects are read through it so an encrypted hint stream can be checked, and objects inside
/// object streams can be found. Unsaved edits in it are ignored. Never panics on hostile input:
/// whatever can't be read is reported as a problem.
pub fn check_linearization(bytes: &[u8], doc: Option<&Document>) -> LinearizationReport {
    let mut report = LinearizationReport { file_length: bytes.len(), ..Default::default() };
    let Some((_, lin, lin_end)) = first_object(bytes) else { return report };
    let Some(version) = lin.get(b"Linearized") else { return report };
    report.linearized = true;
    let int = |k: &[u8]| lin.get(k).and_then(Object::as_int);
    let h: Vec<i64> = lin.get(b"H").and_then(Object::as_array).map(|a| a.iter().filter_map(Object::as_int).collect()).unwrap_or_default();
    let params = LinearizationParams {
        version: version.as_f64(),
        file_length: int(b"L"),
        hint_offset: h.first().copied(),
        hint_length: h.get(1).copied(),
        overflow_hint: match (h.get(2), h.get(3)) {
            (Some(a), Some(b)) => Some((*a, *b)),
            _ => None,
        },
        first_page_object: int(b"O"),
        first_page_end: int(b"E"),
        page_count: int(b"N"),
        main_xref_first_entry: int(b"T"),
    };
    let mut c = Checker {
        data: bytes,
        problems: Vec::new(),
        notes: Vec::new(),
        entries: BTreeMap::new(),
        boundaries: Vec::new(),
        reader: doc.map(Document::original_reader),
        budget: 0,
    };
    check(&mut c, &params, lin_end, &mut report);
    report.params = Some(params);
    report.valid = c.problems.is_empty();
    report.problems = c.problems;
    report.notes = c.notes;
    report
}

fn check(c: &mut Checker, p: &LinearizationParams, lin_end: usize, report: &mut LinearizationReport) {
    let len = c.data.len();
    let as_off = |v: Option<i64>| v.and_then(|v| usize::try_from(v).ok());
    for (name, v) in
        [("/L", p.file_length), ("/O", p.first_page_object), ("/E", p.first_page_end), ("/N", p.page_count), ("/T", p.main_xref_first_entry)]
    {
        if v.is_none_or(|v| v < 0) {
            c.problem(format!("the linearization dictionary has no valid {name}"));
        }
    }
    if p.hint_offset.is_none_or(|v| v < 0) || p.hint_length.is_none_or(|v| v <= 0) {
        c.problem("the linearization dictionary has no valid /H (hint stream offset and length)");
    }
    if p.version.is_none() {
        c.problem("/Linearized is not a number");
    }
    if let Some(l) = p.file_length
        && usize::try_from(l).ok() != Some(len)
    {
        c.problem(format!(
            "/L says the file is {l} bytes but it is {len}: it was changed after it was linearized (an incremental update, or truncated)"
        ));
    }
    if p.overflow_hint.is_some() {
        c.note("the overflow hint stream is not checked");
    }

    // Part 3: the first-page cross-reference section, right after the parameter dictionary.
    let mut lx = Lexer::new(c.data, lin_end);
    lx.skip_ws();
    let fx_at = lx.pos;
    let mut first_entries = BTreeMap::new();
    let first_trailer = match read_xref_section(c.data, fx_at, crate::object::MAX_DECODED, &mut first_entries) {
        Ok((t, _)) => t,
        Err(e) => {
            c.problem(format!("no first-page cross-reference section after the linearization dictionary ({e})"));
            return;
        }
    };
    // The file's last startxref must lead to the first-page section (and through /Prev to the main one).
    let tail = len.saturating_sub(1024);
    let last = c.data.get(tail..).and_then(|t| t.windows(9).rposition(|w| w == b"startxref")).map(|k| tail + k + 9);
    let startxref = last.and_then(|at| Lexer::new(c.data, at).object().ok()).and_then(|o| o.as_int());
    if startxref.and_then(|s| usize::try_from(s).ok()) != Some(fx_at) {
        c.problem(format!(
            "the last startxref ({}) does not point to the first-page cross-reference section at {fx_at}",
            startxref.map_or("missing".into(), |s| s.to_string())
        ));
    }
    let Some(main_at) = first_trailer.int(b"Prev").and_then(|v| usize::try_from(v).ok()) else {
        c.problem("the first-page trailer has no /Prev pointing to the main cross-reference section");
        return;
    };
    let mut main_entries = BTreeMap::new();
    let main_trailer = match read_xref_section(c.data, main_at, crate::object::MAX_DECODED, &mut main_entries) {
        Ok((t, _)) => t,
        Err(e) => {
            c.problem(format!("the main cross-reference section at {main_at} can't be read ({e})"));
            return;
        }
    };
    if main_trailer.get(b"Prev").is_some() {
        c.problem("the main cross-reference section has a /Prev: the file has an earlier revision");
    }
    // /T: the main section's first entry (for a table, the end of line before it).
    match zero_entry(c.data, main_at) {
        Some((ws, entry)) => {
            if let Some(t) = as_off(p.main_xref_first_entry)
                && !(ws <= t && t <= entry)
            {
                c.problem(format!("/T is {t} but the main cross-reference section's first entry starts at {entry}"));
            }
        }
        // A cross-reference stream: its object, or the end of line just before it.
        None if as_off(p.main_xref_first_entry).is_some_and(|t| t <= main_at && t.saturating_add(2) >= main_at) => {}
        None => {
            if let Some(t) = p.main_xref_first_entry {
                c.problem(format!("/T is {t} but the main cross-reference section (at {main_at}) has no entry for object 0 there"));
            }
        }
    }
    c.entries = first_entries.clone();
    for (k, v) in &main_entries {
        c.entries.entry(*k).or_insert(*v);
    }
    let mut boundaries: Vec<usize> =
        c.entries.values().filter_map(|e| if let XrefEntry::InFile { offset, .. } = e { usize::try_from(*offset).ok() } else { None }).collect();
    boundaries.extend([fx_at, main_at, len]);
    if let Some(h) = as_off(p.hint_offset) {
        boundaries.push(h);
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    c.boundaries = boundaries;
    c.budget = (c.entries.len() as u64).saturating_mul(4).saturating_add(1 << 20);

    // The catalog and the pages.
    let Some(root) = first_trailer.reference(b"Root").or_else(|| main_trailer.reference(b"Root")) else {
        c.problem("the first-page trailer has no /Root");
        return;
    };
    if !first_entries.contains_key(&root.num) {
        c.problem(format!("the catalog (object {}) is not in the first-page section", root.num));
    }
    let Some(pages) = c.pages(root) else {
        c.problem("the page tree can't be read");
        return;
    };
    report.pages_found = Some(pages.len());
    if let Some(n) = p.page_count
        && usize::try_from(n).ok() != Some(pages.len())
    {
        c.problem(format!("/N says {n} pages but the page tree has {}", pages.len()));
    }
    let Some(first_page) = pages.first().copied() else {
        c.problem("the document has no pages");
        return;
    };
    if p.first_page_object.is_some_and(|o| o != i64::from(first_page.num)) {
        c.problem(format!("/O is {} but the first page is object {}", p.first_page_object.unwrap_or_default(), first_page.num));
    }
    if !first_entries.contains_key(&first_page.num) {
        c.problem(format!("the first page (object {}) is not in the first-page section", first_page.num));
    }
    // /E: the end of the last object in the first-page section.
    let last_first = first_entries
        .values()
        .filter_map(|e| if let XrefEntry::InFile { offset, .. } = e { usize::try_from(*offset).ok() } else { None })
        .filter(|o| Some(*o) != as_off(p.hint_offset))
        .max();
    if let (Some(e), Some(last)) = (as_off(p.first_page_end), last_first) {
        match c.extent(last) {
            Some((lo, hi)) if lo <= e && e <= hi => {}
            Some((lo, _)) => c.problem(format!("/E is {e} but the first-page section ends at {lo}")),
            None => c.problem("the end of the first-page section can't be found"),
        }
    }
    if let (Some(e), Some(page_at)) = (as_off(p.first_page_end), c.offset(first_page.num))
        && e < page_at
    {
        c.problem("/E is before the first page");
    }

    // /H and the hint tables.
    let (Some(h_off), Some(h_len)) = (as_off(p.hint_offset), as_off(p.hint_length)) else { return };
    if h_off >= len {
        c.problem(format!("/H points past the end of the file ({h_off})"));
        return;
    }
    let Some(hint_ref) = parse_header(c.data, h_off) else {
        c.problem(format!("there is no object at the hint stream offset {h_off}"));
        return;
    };
    if c.offset(hint_ref.num) != Some(h_off) {
        c.problem(format!("the hint stream (object {}) is not listed at offset {h_off}", hint_ref.num));
    } else if !first_entries.contains_key(&hint_ref.num) {
        c.problem("the hint stream is not in the first-page section");
    }
    match c.extent(h_off) {
        Some((lo, hi)) if lo <= h_off.saturating_add(h_len) && h_off.saturating_add(h_len) <= hi => {}
        _ => c.problem(format!("/H says the hint stream is {h_len} bytes, which doesn't match the object at {h_off}")),
    }
    let encrypted = first_trailer.get(b"Encrypt").is_some();
    if encrypted && c.reader.is_none() {
        c.note("the hint stream is encrypted: open the document with its password to check the hint tables");
        return;
    }
    let Some(hint) = c.get(hint_ref) else {
        c.problem("the hint stream can't be read");
        return;
    };
    let Object::Stream(s) = &*hint else {
        c.problem("the object at the hint stream offset is not a stream");
        return;
    };
    let data = match s.decoded_within(64 << 20) {
        Ok(d) => d,
        Err(e) => {
            c.problem(format!("the hint stream can't be decoded ({e})"));
            return;
        }
    };
    let Some(shared_at) = s.dict.int(b"S").and_then(|v| usize::try_from(v).ok()) else {
        c.problem("the hint stream has no /S (shared object hint table offset)");
        return;
    };
    let adjust = |v: u64| if v >= h_off as u64 { v.saturating_add(h_len as u64) } else { v };
    let first_page_group = check_page_table(c, &data, &pages, &adjust);
    match data.get(shared_at..) {
        Some(rest) => check_shared_table(c, rest, first_page.num, first_page_group, &adjust),
        None => c.problem(format!("/S ({shared_at}) is past the end of the hint stream")),
    }
}

/// `N G obj` at `at`.
fn parse_header(data: &[u8], at: usize) -> Option<ObjRef> {
    let mut lx = Lexer::new(data, at);
    lx.skip_ws();
    let num = std::str::from_utf8(lx.token()).ok()?.parse::<u32>().ok()?;
    lx.skip_ws();
    let generation = std::str::from_utf8(lx.token()).ok()?.parse::<u16>().ok()?;
    lx.eat_keyword(b"obj").then_some(ObjRef::new(num, generation))
}

/// In a classic table at `at`: where the subsection for object 0 ends its header line, and
/// where its first entry starts.
fn zero_entry(data: &[u8], at: usize) -> Option<(usize, usize)> {
    let mut lx = Lexer::new(data, at);
    if !lx.eat_keyword(b"xref") {
        return None;
    }
    lx.skip_ws();
    let start = lx.token();
    lx.skip_ws();
    let _count = lx.token();
    if start != b"0" {
        return None;
    }
    let ws = lx.pos;
    lx.skip_ws();
    Some((ws, lx.pos))
}

/// Read Table F.3's 13 header items.
fn page_header(r: &mut BitReader) -> Option<[u64; 13]> {
    let widths = [32, 32, 16, 32, 16, 32, 16, 32, 16, 16, 16, 16, 16];
    let mut out = [0u64; 13];
    for (slot, w) in out.iter_mut().zip(widths) {
        *slot = r.get(w)?;
    }
    Some(out)
}

/// `n` values of `bits` bits each, then padding to the next byte. Values that take no bits
/// cost nothing to read, so `n` must be bounded by the caller.
fn read_group(r: &mut BitReader, n: u64, bits: u64) -> Option<Vec<u64>> {
    let bits = u32::try_from(bits).ok().filter(|b| *b <= 32)?;
    let mut out = Vec::with_capacity(usize::try_from(n).ok()?.min(1 << 20));
    for _ in 0..n {
        out.push(r.get(bits)?);
    }
    r.align();
    Some(out)
}

/// Check the page offset hint table against the pages; returns page 1's object count.
fn check_page_table(c: &mut Checker, data: &[u8], pages: &[ObjRef], adjust: &dyn Fn(u64) -> u64) -> Option<u64> {
    let mut r = BitReader { data, pos: 0 };
    let Some(h) = page_header(&mut r) else {
        c.problem("the page offset hint table is truncated");
        return None;
    };
    let [min_objects, first_loc, objects_bits, min_length, length_bits, _, offset_bits, _, content_bits, refs_bits, id_bits, numerator_bits, _] = h;
    if [objects_bits, length_bits, offset_bits, content_bits, refs_bits, id_bits, numerator_bits].iter().any(|b| *b > 32) {
        c.problem("the page offset hint table has a field wider than 32 bits");
        return None;
    }
    let n = pages.len();
    let groups = (|| {
        let objects = read_group(&mut r, n as u64, objects_bits)?;
        let lengths = read_group(&mut r, n as u64, length_bits)?;
        let refs = read_group(&mut r, n as u64, refs_bits)?;
        Some((objects, lengths, refs))
    })();
    let Some((objects, lengths, refs)) = groups else {
        c.problem("the page offset hint table is truncated");
        return None;
    };
    let objects: Vec<u64> = objects.into_iter().map(|v| v.saturating_add(min_objects)).collect();
    let lengths: Vec<u64> = lengths.into_iter().map(|v| v.saturating_add(min_length)).collect();
    let total_refs = refs.iter().fold(0u64, |a, b| a.saturating_add(*b));
    if total_refs > MAX_SHARED_REFS {
        c.problem(format!("the page offset hint table lists {total_refs} shared object references"));
        return None;
    }
    // Shared object identifiers, then their positions' numerators; content offsets and
    // lengths follow but aren't checked.
    if read_group(&mut r, total_refs, id_bits).is_none() || read_group(&mut r, total_refs, numerator_bits).is_none() {
        c.problem("the page offset hint table is truncated");
        return None;
    }
    // Where each page really is.
    let mut at = first_loc;
    let mut reported = 0;
    for (k, page) in pages.iter().enumerate() {
        let (Some(objs), Some(length)) = (objects.get(k).copied(), lengths.get(k).copied()) else { break };
        match c.offset(page.num) {
            Some(real) if adjust(at) == real as u64 => {}
            Some(real) => {
                if reported < 5 {
                    c.problem(format!("page {}: the hint table puts it at {} but it starts at {real}", k + 1, adjust(at)));
                }
                reported += 1;
            }
            None => c.note(format!("page {} is not a plain object in the file; its offset was not checked", k + 1)),
        }
        if c.offset(page.num).is_some() && c.length_of_run(page.num, objs) != Some(length) && c.budget > 0 && reported < 5 {
            c.problem(format!("page {}: the hint table's {objs} objects and {length} bytes don't match the objects that follow the page", k + 1));
            reported += 1;
        }
        at = at.saturating_add(length);
    }
    objects.first().copied()
}

fn check_shared_table(c: &mut Checker, data: &[u8], first_page: u32, first_page_objects: Option<u64>, adjust: &dyn Fn(u64) -> u64) {
    let mut r = BitReader { data, pos: 0 };
    let header = (|| Some([r.get(32)?, r.get(32)?, r.get(32)?, r.get(32)?, r.get(16)?, r.get(32)?, r.get(16)?]))();
    let Some([first_shared, first_shared_loc, n_first, n_total, objects_bits, min_length, length_bits]) = header else {
        c.problem("the shared object hint table is truncated");
        return;
    };
    if objects_bits > 32 || length_bits > 32 {
        c.problem("the shared object hint table has a field wider than 32 bits");
        return;
    }
    if n_first > n_total || n_total > c.entries.len() as u64 + 1 {
        c.problem(format!("the shared object hint table's counts ({n_first} for page 1, {n_total} in all) are impossible"));
        return;
    }
    if let Some(objs) = first_page_objects
        && objs != n_first
    {
        c.problem(format!("page 1 has {objs} objects in the page offset hint table but {n_first} in the shared object hint table"));
    }
    let mut lengths = Vec::with_capacity(n_total as usize);
    for _ in 0..n_total {
        let Some(v) = r.get(length_bits as u32) else { return c.problem("the shared object hint table is truncated") };
        lengths.push(v.saturating_add(min_length));
    }
    r.align();
    let mut signed = Vec::with_capacity(n_total as usize);
    for _ in 0..n_total {
        let Some(v) = r.get(1) else { return c.problem("the shared object hint table is truncated") };
        signed.push(v == 1);
    }
    r.align();
    for s in &signed {
        if *s && r.get(64).zip(r.get(64)).is_none() {
            return c.problem("the shared object hint table is truncated");
        }
    }
    let mut counts = Vec::with_capacity(n_total as usize);
    for _ in 0..n_total {
        let Some(v) = r.get(objects_bits as u32) else { return c.problem("the shared object hint table is truncated") };
        counts.push(v.saturating_add(1));
    }
    if n_total > n_first {
        let Ok(first_shared) = u32::try_from(first_shared) else { return c.problem("the first shared object number is out of range") };
        match c.offset(first_shared) {
            Some(real) if adjust(first_shared_loc) == real as u64 => {}
            Some(real) => {
                c.problem(format!("the shared object hint table puts object {first_shared} at {} but it starts at {real}", adjust(first_shared_loc)))
            }
            None => c.problem(format!("the first shared object ({first_shared}) is not in the file")),
        }
    }
    let mut num = u64::from(first_page);
    let mut reported = 0;
    for (k, (length, count)) in lengths.iter().zip(&counts).enumerate() {
        if k as u64 == n_first {
            num = first_shared;
        }
        let real = u32::try_from(num).ok().and_then(|n| c.length_of_run(n, *count));
        if real != Some(*length) && c.budget > 0 {
            if reported < 5 {
                c.problem(format!(
                    "shared object group {} (object {num}): the hint table says {length} bytes, the file has {}",
                    k + 1,
                    real.map_or("no such objects".into(), |r| r.to_string())
                ));
            }
            reported += 1;
        }
        num = num.saturating_add(*count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_round_trip_across_byte_boundaries() {
        let values = [(5u64, 3u32), (0, 0), (1, 1), (0xABCD, 16), (0xFFFF_FFFF, 32), (2, 7)];
        let mut w = BitWriter::default();
        for (v, b) in values {
            w.put(v, b);
        }
        w.align();
        let mut r = BitReader { data: &w.out, pos: 0 };
        for (v, b) in values {
            assert_eq!(r.get(b), Some(v));
        }
        r.align();
        assert_eq!(r.get(1), None, "nothing past the padding");
        assert_eq!(bits_for(0), 0);
        assert_eq!(bits_for(1), 1);
        assert_eq!(bits_for(255), 8);
        assert_eq!(bits_for(256), 9);
    }

    #[test]
    fn groups_refuse_wide_fields_and_truncation() {
        let data = [0xFFu8; 4];
        assert!(read_group(&mut BitReader { data: &data, pos: 0 }, 1, 33).is_none());
        assert!(read_group(&mut BitReader { data: &data, pos: 0 }, 2, 32).is_none());
        assert_eq!(read_group(&mut BitReader { data: &data, pos: 0 }, 1_000_000, 0).map(|v| v.len()), Some(1_000_000));
    }
}
