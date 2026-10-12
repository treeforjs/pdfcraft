//! Destinations (ISO 32000-2 §12.3.2): explicit views and named destinations.
//!
//! An **explicit destination** is an array `[page /Type args…]` naming a page and how to show it
//! (`/XYZ left top zoom`, `/Fit`, `/FitH top`, `/FitV left`, `/FitR l b r t`, `/FitB`,
//! `/FitBH top`, `/FitBV left`). A **named destination** maps a name to one, either through the
//! `/Names /Dests` name tree (keys are strings, PDF 1.2+) or the legacy catalog `/Dests`
//! dictionary (keys are names, PDF 1.1). A link or bookmark refers to the first with a string and
//! to the second with a name object.
//!
//! Reading is lenient and bounded: name trees are walked with a seen-set, a depth limit and an
//! entry cap, and an unknown or missing fit type reads as `/Fit` (what viewers show). Writing is
//! atomic: a name tree that could not be read completely (too large, cyclic `/Kids`, malformed
//! pairs) is never rewritten, because that would drop entries. A rewritten tree is a single flat
//! node (`/Names [key value …]`, sorted by key bytes), which every reader accepts.
//!
//! Coordinates are PDF user space, exactly as stored in the destination (and as `pdfinfo -dests`
//! prints them). Zoom is a factor (1.0 = 100%).

use std::collections::HashSet;

use pdfcraft_cos::{Dict, Document, ObjRef, Object, PdfString};

/// Deepest name-tree nesting followed.
const MAX_DEPTH: usize = 64;
/// Most name-tree entries read.
const MAX_ENTRIES: usize = 200_000;
/// Most name-tree nodes and outline items visited.
const MAX_NODES: usize = 200_000;
/// Longest destination name accepted for a new or renamed destination (characters).
pub const MAX_NAME_CHARS: usize = 512;
/// Largest coordinate magnitude accepted when writing (far beyond any real page, finite).
const MAX_COORD: f64 = 1.0e7;
/// Largest zoom factor accepted when writing (6400%).
pub const MAX_ZOOM: f64 = 64.0;

/// How a destination shows its page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fit {
    /// `/XYZ left top zoom`: `None` keeps the reader's current value.
    Xyz { left: Option<f64>, top: Option<f64>, zoom: Option<f64> },
    /// `/Fit`: the whole page.
    Fit,
    /// `/FitH top`: the page width, with `top` at the top of the window.
    FitH { top: Option<f64> },
    /// `/FitV left`: the page height, with `left` at the left of the window.
    FitV { left: Option<f64> },
    /// `/FitR left bottom right top`: this rectangle.
    FitR { rect: [f64; 4] },
    /// `/FitB`: the page's bounding box.
    FitB,
    /// `/FitBH top`: the bounding box's width.
    FitBH { top: Option<f64> },
    /// `/FitBV left`: the bounding box's height.
    FitBV { left: Option<f64> },
}

impl Fit {
    /// The PDF name of the fit type (`XYZ`, `Fit`, `FitH`, …).
    pub fn kind(&self) -> &'static str {
        match self {
            Fit::Xyz { .. } => "XYZ",
            Fit::Fit => "Fit",
            Fit::FitH { .. } => "FitH",
            Fit::FitV { .. } => "FitV",
            Fit::FitR { .. } => "FitR",
            Fit::FitB => "FitB",
            Fit::FitBH { .. } => "FitBH",
            Fit::FitBV { .. } => "FitBV",
        }
    }

    /// Build a fit from its PDF name (case-insensitive) and the values it uses. Values a fit type
    /// doesn't use must be absent, so a misplaced argument is an error rather than ignored.
    pub fn from_parts(kind: &str, left: Option<f64>, top: Option<f64>, zoom: Option<f64>, rect: Option<[f64; 4]>) -> Result<Fit> {
        let k = kind.to_ascii_lowercase();
        let unused = |names: &[(&str, bool)]| -> Result<()> {
            match names.iter().find(|(_, present)| *present) {
                Some((n, _)) => Err(DestError::Invalid(format!("{n} does not apply to a {kind} destination"))),
                None => Ok(()),
            }
        };
        let (l, t, z, r) = (("left", left.is_some()), ("top", top.is_some()), ("zoom", zoom.is_some()), ("rect", rect.is_some()));
        let fit = match k.as_str() {
            "xyz" => {
                unused(&[r])?;
                Fit::Xyz { left, top, zoom }
            }
            "fit" => {
                unused(&[l, t, z, r])?;
                Fit::Fit
            }
            "fith" => {
                unused(&[l, z, r])?;
                Fit::FitH { top }
            }
            "fitv" => {
                unused(&[t, z, r])?;
                Fit::FitV { left }
            }
            "fitr" => {
                unused(&[l, t, z])?;
                Fit::FitR { rect: rect.ok_or_else(|| DestError::Invalid("a FitR destination needs rect [left, bottom, right, top]".into()))? }
            }
            "fitb" => {
                unused(&[l, t, z, r])?;
                Fit::FitB
            }
            "fitbh" => {
                unused(&[l, z, r])?;
                Fit::FitBH { top }
            }
            "fitbv" => {
                unused(&[t, z, r])?;
                Fit::FitBV { left }
            }
            _ => return Err(DestError::Invalid(format!("unknown fit {kind:?}: use XYZ, Fit, FitH, FitV, FitR, FitB, FitBH or FitBV"))),
        };
        Ok(fit)
    }
}

/// A page of this document and how to show it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    /// 0-based page index.
    pub page: usize,
    pub fit: Fit,
}

impl View {
    /// The page, fitting the window (`/Fit`).
    pub fn page(page: usize) -> Self {
        View { page, fit: Fit::Fit }
    }
}

/// What a go-to action targets.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    View(View),
    /// A named destination (it may not exist: a dangling name is kept as is).
    Named(String),
}

/// Where a named destination is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DestSource {
    /// The `/Names /Dests` name tree (string keys).
    NameTree,
    /// The legacy catalog `/Dests` dictionary (name keys).
    Legacy,
}

/// A named destination.
#[derive(Clone, Debug, PartialEq)]
pub struct NamedDest {
    pub name: String,
    pub source: DestSource,
    /// The view it opens, or `None` when it doesn't resolve to a page of this document.
    pub view: Option<View>,
}

#[derive(Debug, PartialEq)]
pub enum DestError {
    NoSuchName(String),
    Exists(String),
    NoSuchPage(usize),
    Invalid(String),
    /// The name tree could not be read completely; rewriting it would lose entries.
    Unwritable(String),
    Cos(String),
}

impl std::fmt::Display for DestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DestError::NoSuchName(n) => write!(f, "there is no named destination {n:?}"),
            DestError::Exists(n) => write!(f, "a named destination {n:?} already exists"),
            DestError::NoSuchPage(p) => write!(f, "there is no page {}", p.saturating_add(1)),
            DestError::Invalid(m) => f.write_str(m),
            DestError::Unwritable(m) => write!(f, "the named destinations can't be changed safely: {m}"),
            DestError::Cos(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for DestError {}

impl From<pdfcraft_cos::CosError> for DestError {
    fn from(e: pdfcraft_cos::CosError) -> Self {
        DestError::Cos(e.to_string())
    }
}

type Result<T> = std::result::Result<T, DestError>;

/// The page objects, in order.
pub fn page_refs(doc: &Document) -> Vec<ObjRef> {
    crate::pages(doc).into_iter().map(|p| p.obj).collect()
}

/// A destination name's text (keys are byte strings: UTF-16BE with BOM, UTF-8, or PDFDocEncoding).
pub fn name_text(raw: &[u8]) -> String {
    PdfString::literal(raw.to_vec()).to_text()
}

fn finite(o: Option<&Object>, doc: &Document) -> Option<f64> {
    o.map(|o| doc.resolve(o)).and_then(|o| o.as_f64()).filter(|v| v.is_finite())
}

/// Read an explicit destination array. Unknown or missing fit types read as `/Fit`.
pub fn parse_view(doc: &Document, arr: &[Object], pages: &[ObjRef]) -> Option<View> {
    let page = match arr.first()? {
        Object::Ref(r) => pages.iter().position(|p| p == r)?,
        // Page numbers belong to remote destinations; some writers use them locally anyway.
        Object::Int(i) => usize::try_from(*i).ok().filter(|i| *i < pages.len())?,
        _ => return None,
    };
    let n = |i: usize| finite(arr.get(i), doc);
    let kind = arr.get(1).map(|k| doc.resolve(k)).and_then(|k| k.as_name().map(<[u8]>::to_vec)).unwrap_or_default();
    let fit = match kind.as_slice() {
        b"XYZ" => Fit::Xyz { left: n(2), top: n(3), zoom: n(4).filter(|z| *z > 0.0) },
        b"FitH" => Fit::FitH { top: n(2) },
        b"FitV" => Fit::FitV { left: n(2) },
        b"FitR" => match (n(2), n(3), n(4), n(5)) {
            (Some(l), Some(b), Some(r), Some(t)) => Fit::FitR { rect: [l.min(r), b.min(t), l.max(r), b.max(t)] },
            _ => Fit::Fit,
        },
        b"FitB" => Fit::FitB,
        b"FitBH" => Fit::FitBH { top: n(2) },
        b"FitBV" => Fit::FitBV { left: n(2) },
        _ => Fit::Fit,
    };
    Some(View { page, fit })
}

/// Read a named-destination value: an explicit array, or a dictionary whose `/D` is one.
fn value_view(doc: &Document, v: &Object, pages: &[ObjRef]) -> Option<View> {
    let v = doc.resolve(v);
    match &*v {
        Object::Array(a) => parse_view(doc, a, pages),
        Object::Dict(d) => {
            let inner = doc.resolve(d.get(b"D")?);
            parse_view(doc, inner.as_array()?, pages)
        }
        _ => None,
    }
}

/// What a `/Dest` entry or a GoTo action's `/D` targets.
pub fn read_target(doc: &Document, o: &Object, pages: &[ObjRef]) -> Option<Target> {
    let o = doc.resolve(o);
    match &*o {
        Object::Array(a) => parse_view(doc, a, pages).map(Target::View),
        Object::String(s) => Some(Target::Named(name_text(&s.bytes))),
        Object::Name(n) => Some(Target::Named(name_text(n))),
        _ => None,
    }
}

fn check_coord(v: Option<f64>, what: &str) -> Result<()> {
    match v {
        Some(x) if !x.is_finite() || x.abs() > MAX_COORD => Err(DestError::Invalid(format!("{what} must be a finite coordinate"))),
        _ => Ok(()),
    }
}

fn num(v: Option<f64>) -> Object {
    v.map_or(Object::Null, Object::Real)
}

/// The explicit destination array for `view`, validated.
pub fn view_object(view: &View, pages: &[ObjRef]) -> Result<Object> {
    let p = *pages.get(view.page).ok_or(DestError::NoSuchPage(view.page))?;
    let mut a = vec![Object::Ref(p), Object::name(view.fit.kind())];
    match view.fit {
        Fit::Xyz { left, top, zoom } => {
            check_coord(left, "left")?;
            check_coord(top, "top")?;
            // A zoom of 0 means "keep the current zoom", like null (§12.3.2.2).
            let zoom = zoom.filter(|z| *z != 0.0);
            if let Some(z) = zoom
                && !(z.is_finite() && z > 0.0 && z <= MAX_ZOOM)
            {
                return Err(DestError::Invalid(format!("zoom must be between 0 and {}%", MAX_ZOOM * 100.0)));
            }
            a.extend([num(left), num(top), num(zoom)]);
        }
        Fit::Fit | Fit::FitB => {}
        Fit::FitH { top } | Fit::FitBH { top } => {
            check_coord(top, "top")?;
            a.push(num(top));
        }
        Fit::FitV { left } | Fit::FitBV { left } => {
            check_coord(left, "left")?;
            a.push(num(left));
        }
        Fit::FitR { rect } => {
            for v in rect {
                check_coord(Some(v), "rect")?;
            }
            let r = [rect[0].min(rect[2]), rect[1].min(rect[3]), rect[0].max(rect[2]), rect[1].max(rect[3])];
            if r[2] - r[0] <= 0.0 || r[3] - r[1] <= 0.0 {
                return Err(DestError::Invalid("the FitR rectangle must have a width and a height".into()));
            }
            a.extend(r.iter().map(|v| Object::Real(*v)));
        }
    }
    Ok(Object::Array(a))
}

// ── storage ─────────────────────────────────────────────────────────────────────────────────

/// The name tree's entries in stored order, and whether they were all read.
struct Tree {
    entries: Vec<(PdfString, Object)>,
    complete: bool,
    problem: String,
}

fn catalog(doc: &Document) -> Option<Dict> {
    doc.get(doc.root()?).as_dict().cloned()
}

/// The catalog's `/Names /Dests` entry (unresolved), if any.
fn tree_root(doc: &Document) -> Option<Object> {
    let names = doc.resolve(catalog(doc)?.get(b"Names")?);
    names.as_dict()?.get(b"Dests").cloned()
}

fn read_tree(doc: &Document) -> Tree {
    let mut t = Tree { entries: Vec::new(), complete: true, problem: String::new() };
    let Some(root) = tree_root(doc) else { return t };
    fn walk(doc: &Document, node: &Object, depth: usize, seen: &mut HashSet<ObjRef>, t: &mut Tree) {
        if depth > MAX_DEPTH {
            t.complete = false;
            t.problem = "the name tree is nested too deeply".into();
            return;
        }
        if let Object::Ref(r) = node
            && (seen.len() >= MAX_NODES || !seen.insert(*r))
        {
            t.complete = false;
            t.problem = "the name tree has a loop or too many nodes".into();
            return;
        }
        let n = doc.resolve(node);
        let Some(d) = n.as_dict() else {
            t.complete = false;
            t.problem = "a name tree node is not a dictionary".into();
            return;
        };
        if let Some(names) = d.get(b"Names") {
            let names = doc.resolve(names);
            let Some(arr) = names.as_array() else {
                t.complete = false;
                t.problem = "a name tree /Names entry is not an array".into();
                return;
            };
            let (pairs, rest) = arr.as_chunks::<2>();
            if !rest.is_empty() {
                t.complete = false;
                t.problem = "a name tree /Names array has an odd length".into();
            }
            for [k, v] in pairs {
                if t.entries.len() >= MAX_ENTRIES {
                    t.complete = false;
                    t.problem = format!("there are more than {MAX_ENTRIES} named destinations");
                    return;
                }
                match doc.resolve(k).as_string() {
                    Some(s) => t.entries.push((s.clone(), v.clone())),
                    None => {
                        t.complete = false;
                        t.problem = "a name tree key is not a string".into();
                    }
                }
            }
        }
        if let Some(kids) = d.get(b"Kids") {
            let kids = doc.resolve(kids);
            for k in kids.as_array().map(Vec::as_slice).unwrap_or_default() {
                walk(doc, k, depth + 1, seen, t);
            }
        }
    }
    walk(doc, &root, 0, &mut HashSet::new(), &mut t);
    t
}

/// The legacy `/Dests` dictionary (entries in stored order).
fn read_legacy(doc: &Document) -> Option<Dict> {
    let c = catalog(doc)?;
    doc.resolve(c.get(b"Dests")?).as_dict().cloned()
}

/// Every named destination: name-tree entries (in stored order), then legacy ones.
pub fn list(doc: &Document) -> Vec<NamedDest> {
    let pages = page_refs(doc);
    let mut out: Vec<NamedDest> = read_tree(doc)
        .entries
        .iter()
        .map(|(k, v)| NamedDest { name: name_text(&k.bytes), source: DestSource::NameTree, view: value_view(doc, v, &pages) })
        .collect();
    if let Some(d) = read_legacy(doc) {
        for (k, v) in d.iter().take(MAX_ENTRIES) {
            out.push(NamedDest { name: name_text(k), source: DestSource::Legacy, view: value_view(doc, v, &pages) });
        }
    }
    out
}

fn matches(raw: &[u8], name: &str) -> bool {
    raw == name.as_bytes() || name_text(raw) == name
}

/// The named destination called `name` (the name tree wins over the legacy dictionary).
pub fn lookup(doc: &Document, name: &str) -> Option<NamedDest> {
    let pages = page_refs(doc);
    if let Some((k, v)) = read_tree(doc).entries.iter().find(|(k, _)| matches(&k.bytes, name)) {
        return Some(NamedDest { name: name_text(&k.bytes), source: DestSource::NameTree, view: value_view(doc, v, &pages) });
    }
    let d = read_legacy(doc)?;
    let (k, v) = d.iter().find(|(k, _)| matches(k, name))?;
    Some(NamedDest { name: name_text(k), source: DestSource::Legacy, view: value_view(doc, v, &pages) })
}

/// The `/Dest` value that refers to the named destination `name`: a string for the name tree, a
/// name object for the legacy dictionary (§12.3.2.3).
pub fn named_ref(doc: &Document, name: &str) -> Result<Object> {
    if let Some((k, _)) = read_tree(doc).entries.iter().find(|(k, _)| matches(&k.bytes, name)) {
        return Ok(Object::String(k.clone()));
    }
    if let Some(d) = read_legacy(doc)
        && let Some((k, _)) = d.iter().find(|(k, _)| matches(k, name))
    {
        return Ok(Object::Name(k.clone()));
    }
    Err(DestError::NoSuchName(name.to_string()))
}

/// The `/Dest` value for a go-to target.
pub fn target_object(doc: &Document, target: &Target) -> Result<Object> {
    match target {
        Target::View(v) => view_object(v, &page_refs(doc)),
        Target::Named(n) => named_ref(doc, n),
    }
}

fn writable_tree(doc: &Document) -> Result<Vec<(PdfString, Object)>> {
    let t = read_tree(doc);
    if !t.complete {
        return Err(DestError::Unwritable(t.problem));
    }
    Ok(t.entries)
}

/// Replace the name tree with one flat node holding `entries` (sorted by key bytes).
fn write_tree(doc: &mut Document, mut entries: Vec<(PdfString, Object)>) -> Result<()> {
    let root = doc.root().ok_or_else(|| DestError::Invalid("the document has no catalog".into()))?;
    entries.sort_by(|a, b| a.0.bytes.cmp(&b.0.bytes));
    let old = tree_root(doc);
    let tree = if entries.is_empty() {
        None
    } else {
        let mut d = Dict::new();
        d.set(b"Names".to_vec(), Object::Array(entries.into_iter().flat_map(|(k, v)| [Object::String(k), v]).collect()));
        Some(match old {
            // Reuse the tree's object: the old kids become unreferenced.
            Some(Object::Ref(r)) => {
                doc.set(r, Object::Dict(d));
                Object::Ref(r)
            }
            _ => Object::Ref(doc.add(Object::Dict(d))),
        })
    };
    let edit = |n: &mut Dict| match &tree {
        Some(t) => n.set(b"Dests".to_vec(), t.clone()),
        None => {
            n.remove(b"Dests");
        }
    };
    match catalog(doc).and_then(|c| c.get(b"Names").cloned()) {
        Some(Object::Ref(r)) if doc.get(r).as_dict().is_some() => doc.update_dict(r, edit)?,
        Some(Object::Dict(_)) => doc.update_dict(root, |c| {
            if let Some(n) = c.get_mut(b"Names").and_then(Object::as_dict_mut) {
                edit(n);
            }
        })?,
        _ if tree.is_some() => {
            let mut n = Dict::new();
            edit(&mut n);
            doc.update_dict(root, |c| c.set(b"Names".to_vec(), Object::Dict(n)))?;
        }
        _ => {}
    }
    Ok(())
}

fn write_legacy(doc: &mut Document, dests: Dict) -> Result<()> {
    let root = doc.root().ok_or_else(|| DestError::Invalid("the document has no catalog".into()))?;
    match catalog(doc).and_then(|c| c.get(b"Dests").cloned()) {
        Some(Object::Ref(r)) => doc.set(r, Object::Dict(dests)),
        _ => doc.update_dict(root, |c| c.set(b"Dests".to_vec(), Object::Dict(dests)))?,
    }
    Ok(())
}

/// A named-destination value pointing at `dest`: keeps a dictionary form (`<< /D … >>` with other
/// keys such as `/SD`) when the old value had one.
fn retarget(doc: &Document, old: &Object, dest: Object) -> Object {
    match doc.resolve(old).as_dict() {
        Some(d) if d.contains(b"D") => {
            let mut d = d.clone();
            d.set(b"D".to_vec(), dest);
            Object::Dict(d)
        }
        _ => dest,
    }
}

fn check_name(name: &str) -> Result<()> {
    if name.trim().is_empty() {
        return Err(DestError::Invalid("the destination name must not be empty".into()));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(DestError::Invalid(format!("the destination name is longer than {MAX_NAME_CHARS} characters")));
    }
    if name.chars().any(char::is_control) {
        return Err(DestError::Invalid("the destination name must not contain control characters".into()));
    }
    Ok(())
}

/// Create the named destination `name`, or re-target it if it exists (where it is stored).
/// Returns `true` when it was created (new names go into the name tree).
pub fn set(doc: &mut Document, name: &str, view: &View) -> Result<bool> {
    let dest = view_object(view, &page_refs(doc))?;
    let mut tree = writable_tree(doc)?;
    if let Some(slot) = tree.iter_mut().find(|(k, _)| matches(&k.bytes, name)) {
        slot.1 = retarget(doc, &slot.1, dest);
        write_tree(doc, tree)?;
        return Ok(false);
    }
    let mut legacy = read_legacy(doc).unwrap_or_default();
    let key = legacy.iter().find(|(k, _)| matches(k, name)).map(|(k, _)| k.clone());
    if let Some(key) = key {
        let old = legacy.get(&key).cloned().unwrap_or(Object::Null);
        legacy.set(key, retarget(doc, &old, dest));
        write_legacy(doc, legacy)?;
        return Ok(false);
    }
    check_name(name)?;
    tree.push((PdfString::text(name), dest));
    write_tree(doc, tree)?;
    Ok(true)
}

// ── references from links and bookmarks ─────────────────────────────────────────────────────

/// Where a go-to destination value lives.
enum Slot {
    /// `/Dest` of a link or bookmark.
    Dest(ObjRef),
    /// `/A /D` of an inline action dictionary.
    InlineAction(ObjRef),
    /// `/D` of an indirect action object.
    Action(ObjRef),
}

/// Link annotations (indirect ones) and bookmark items, bounded.
fn holders(doc: &Document) -> Vec<ObjRef> {
    let mut out = Vec::new();
    for p in crate::pages(doc) {
        let annots = p.dict.get(b"Annots").map(|a| doc.resolve(a));
        for a in annots.as_deref().and_then(Object::as_array).map(Vec::as_slice).unwrap_or_default() {
            if out.len() >= MAX_NODES {
                return out;
            }
            if let Object::Ref(r) = a
                && doc.get(*r).as_dict().and_then(|d| d.name(b"Subtype").map(<[u8]>::to_vec)).as_deref() == Some(b"Link")
            {
                out.push(*r);
            }
        }
    }
    let Some(outlines) = catalog(doc).and_then(|c| c.reference(b"Outlines")) else { return out };
    let mut seen = HashSet::from([outlines]);
    let mut stack = vec![outlines];
    while let Some(node) = stack.pop() {
        let mut next = doc.get(node).as_dict().and_then(|d| d.reference(b"First"));
        while let Some(r) = next {
            if seen.len() >= MAX_NODES || !seen.insert(r) {
                break;
            }
            let item = doc.get(r);
            let Some(d) = item.as_dict() else { break };
            out.push(r);
            stack.push(r);
            next = d.reference(b"Next");
        }
    }
    out
}

fn slot_of(doc: &Document, holder: ObjRef) -> Option<(Slot, Object)> {
    let h = doc.get(holder);
    let d = h.as_dict()?;
    if let Some(dest) = d.get(b"Dest") {
        return Some((Slot::Dest(holder), dest.clone()));
    }
    let a = d.get(b"A")?;
    let action = doc.resolve(a);
    let ad = action.as_dict()?;
    if ad.name(b"S") != Some(b"GoTo") {
        return None;
    }
    let dest = ad.get(b"D")?.clone();
    Some((if let Object::Ref(r) = a { Slot::Action(*r) } else { Slot::InlineAction(holder) }, dest))
}

fn write_slot(doc: &mut Document, slot: &Slot, value: Object) -> Result<()> {
    match slot {
        Slot::Dest(r) => doc.update_dict(*r, |d| d.set(b"Dest".to_vec(), value))?,
        Slot::Action(r) => doc.update_dict(*r, |d| d.set(b"D".to_vec(), value))?,
        Slot::InlineAction(r) => doc.update_dict(*r, |d| {
            if let Some(a) = d.get_mut(b"A").and_then(Object::as_dict_mut) {
                a.set(b"D".to_vec(), value);
            }
        })?,
    }
    Ok(())
}

/// Does `value` refer to the entry with key `raw` in `source`? Strings refer to the name tree and
/// names to the legacy dictionary; `either` also accepts the other kind (lenient readers do,
/// and it is unambiguous when the other store has no such key).
fn refers(doc: &Document, value: &Object, raw: &[u8], source: DestSource, either: bool) -> bool {
    match &*doc.resolve(value) {
        Object::String(s) => s.bytes == raw && (source == DestSource::NameTree || either),
        Object::Name(n) => n == raw && (source == DestSource::Legacy || either),
        _ => false,
    }
}

/// Where the named destination `name` is stored, its key bytes, and whether references of the
/// other kind (string vs name) are unambiguous.
fn locate(doc: &Document, name: &str) -> Option<(DestSource, Vec<u8>, bool)> {
    let tree = read_tree(doc).entries;
    let legacy = read_legacy(doc).unwrap_or_default();
    if let Some((k, _)) = tree.iter().find(|(k, _)| matches(&k.bytes, name)) {
        let either = legacy.get(&k.bytes).is_none();
        return Some((DestSource::NameTree, k.bytes.clone(), either));
    }
    let (k, _) = legacy.iter().find(|(k, _)| matches(k, name))?;
    let either = !tree.iter().any(|(t, _)| t.bytes == *k);
    Some((DestSource::Legacy, k.clone(), either))
}

/// How many links and bookmarks go to the named destination `name`.
pub fn references(doc: &Document, name: &str) -> usize {
    let Some((source, raw, either)) = locate(doc, name) else {
        // A deleted name: count references by its bytes in either form.
        let raw = name.as_bytes();
        return holders(doc).iter().filter_map(|h| slot_of(doc, *h)).filter(|(_, v)| refers(doc, v, raw, DestSource::NameTree, true)).count();
    };
    holders(doc).iter().filter_map(|h| slot_of(doc, *h)).filter(|(_, v)| refers(doc, v, &raw, source, either)).count()
}

/// Rename a named destination, and point the links and bookmarks that used the old name at the
/// new one. Returns how many references were updated.
pub fn rename(doc: &mut Document, from: &str, to: &str) -> Result<usize> {
    check_name(to)?;
    let (source, raw, either) = locate(doc, from).ok_or_else(|| DestError::NoSuchName(from.to_string()))?;
    if locate(doc, to).is_some_and(|(s, r, _)| !(s == source && r == raw)) {
        return Err(DestError::Exists(to.to_string()));
    }
    let new_ref = match source {
        DestSource::NameTree => {
            let mut tree = writable_tree(doc)?;
            let key = PdfString::text(to);
            for (k, _) in tree.iter_mut().filter(|(k, _)| k.bytes == raw) {
                *k = key.clone();
            }
            write_tree(doc, tree)?;
            Object::String(key)
        }
        DestSource::Legacy => {
            let old = read_legacy(doc).unwrap_or_default();
            let key = to.as_bytes().to_vec();
            // Rebuild in order so the renamed entry keeps its position.
            let renamed: Dict = old.iter().map(|(k, v)| (if *k == raw { key.clone() } else { k.clone() }, v.clone())).collect();
            write_legacy(doc, renamed)?;
            Object::Name(key)
        }
    };
    let mut n = 0;
    for h in holders(doc) {
        if let Some((slot, v)) = slot_of(doc, h)
            && refers(doc, &v, &raw, source, either)
        {
            write_slot(doc, &slot, new_ref.clone())?;
            n += 1;
        }
    }
    Ok(n)
}

/// Delete a named destination. Links and bookmarks that used it keep the (now dangling) name, as
/// in Acrobat. Returns how many still refer to it.
pub fn delete(doc: &mut Document, name: &str) -> Result<usize> {
    let (source, raw, either) = locate(doc, name).ok_or_else(|| DestError::NoSuchName(name.to_string()))?;
    let still = holders(doc).iter().filter_map(|h| slot_of(doc, *h)).filter(|(_, v)| refers(doc, v, &raw, source, either)).count();
    match source {
        DestSource::NameTree => {
            let mut tree = writable_tree(doc)?;
            tree.retain(|(k, _)| k.bytes != raw);
            write_tree(doc, tree)?;
        }
        DestSource::Legacy => {
            let mut legacy = read_legacy(doc).unwrap_or_default();
            legacy.remove(&raw);
            write_legacy(doc, legacy)?;
        }
    }
    Ok(still)
}

#[cfg(test)]
mod tests;
