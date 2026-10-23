//! The Reading Order tool and the Order panel: tag what lies inside a rectangle on a page as
//! one kind of content (Text, a heading, Figure, Figure/Caption, Table, Cell, Formula, Form
//! field, or Background, which is an artifact), list the page's reading order (the regions the
//! structure tree reads, in order, with where each is drawn), move an element before or after
//! another, and clear a page's structure.
//!
//! Tagging a region works on painting operations ("units": one text-showing operator, one
//! painted path, one image, form XObject or shading). A unit is inside the rectangle when the
//! centre of its box is: text by its glyphs (the caller's text extractor reads a provisional
//! copy of the page in which every text unit has a marked-content id of its own, as
//! Autotag does), paths by their points, images and forms by their boxes. Then:
//!
//! - untagged units are wrapped in new marked content with new MCIDs;
//! - tagged (MCID) or artifact marked content wholly inside changes as a whole: its opening
//!   operator gets the new tag and keeps its MCID (an artifact gets a new one), and the MCID
//!   moves to the new element;
//! - marked content only partly inside is split: its `BDC`/`EMC` pair goes, and its units are
//!   wrapped again run by run, the units outside keeping the old tag and element (the first
//!   run keeps the old MCID, later runs get new ones in the same place of the old element's
//!   kids) and the units inside going to the new element.
//!
//! New sequences open and close between whole operations, never across `q`, `Q`, `BT`, `ET`,
//! other marked content or a path, so the page's nesting is kept. Elements left empty leave the
//! tree, the new element goes next to the element of the nearest tagged content before it on
//! the page (after it, else at the end of the top-level grouping element), and the page's
//! parent-tree entry is rebuilt. An untagged document gets a structure tree (a `Document`
//! element, the parent tree and `/MarkInfo`). Every change is made on a copy and kept only when
//! all of it succeeds; every walk is bounded.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use pdfcraft_content::{Matrix, Op};
use pdfcraft_cos::{Dict, Document, ObjRef, Object, PdfString, Stream};

use crate::artifact::{ArtifactSubtype, ArtifactType};
use crate::autotag::{AutotagGlyph, AutotagText, view_of};
use crate::keep::{
    Elements, GROUPING, insert_kid, kid_list, mcr_mcid, number_max, number_tree_nodes, nums, parent_tree, place_element, prune, put_number,
    rebuild_parent_tree, root_ref,
};
use crate::tags::{MAX_MCID, MAX_TEXT, TagError, TagKid, Tags, catalog, page_content, sub_dict, tags};

/// Pages with more painting operations than this are refused.
pub const MAX_REGION_UNITS: usize = 100_000;
/// Marked content nested deeper than this is refused.
const MAX_NESTING: usize = 4096;
/// Graphics-state nesting followed this deep.
const MAX_GSTATE: usize = 4096;
/// A table region makes at most this many cells.
const MAX_TABLE_CELLS: usize = 10_000;
/// Widgets looked at on one page.
const MAX_WIDGETS: usize = 10_000;
/// Slack when testing that a box's centre lies inside the rectangle (points).
const EPS: f64 = 0.5;

/// What the Reading Order tool tags a region as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionRole {
    /// Text/Paragraph: a `P` element.
    Text,
    /// Heading 1–6: `H1`…`H6`.
    Heading(u8),
    /// A `Figure` (give it alternate text).
    Figure,
    /// Figure/Caption: a `Figure` holding the pictures and drawings, with a `Caption` element
    /// for the text inside it.
    FigureCaption,
    /// A `Table`: rows (`TR`) and cells found from where the content lies, the first row as
    /// header cells (`TH`, column scope); rules and other drawings become layout artifacts.
    Table,
    /// One table cell (`TD`), placed in the row of the nearest cell before it when there is one.
    Cell,
    /// A `Formula` (give it alternate text).
    Formula,
    /// Form field: each widget inside becomes a `Form` element with an object reference.
    FormField,
    /// Background/Artifact: the content becomes an artifact.
    Background,
}

impl RegionRole {
    /// The ids automation uses.
    pub const IDS: [&'static str; 14] =
        ["text", "h1", "h2", "h3", "h4", "h5", "h6", "figure", "figure-caption", "table", "cell", "formula", "form-field", "background"];

    pub fn id(self) -> &'static str {
        match self {
            RegionRole::Text => "text",
            RegionRole::Heading(1) => "h1",
            RegionRole::Heading(2) => "h2",
            RegionRole::Heading(3) => "h3",
            RegionRole::Heading(4) => "h4",
            RegionRole::Heading(5) => "h5",
            RegionRole::Heading(_) => "h6",
            RegionRole::Figure => "figure",
            RegionRole::FigureCaption => "figure-caption",
            RegionRole::Table => "table",
            RegionRole::Cell => "cell",
            RegionRole::Formula => "formula",
            RegionRole::FormField => "form-field",
            RegionRole::Background => "background",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase();
        Some(match s.as_str() {
            "text" | "p" | "paragraph" => RegionRole::Text,
            "h1" => RegionRole::Heading(1),
            "h2" => RegionRole::Heading(2),
            "h3" => RegionRole::Heading(3),
            "h4" => RegionRole::Heading(4),
            "h5" => RegionRole::Heading(5),
            "h6" => RegionRole::Heading(6),
            "figure" => RegionRole::Figure,
            "figure-caption" | "figure/caption" => RegionRole::FigureCaption,
            "table" => RegionRole::Table,
            "cell" | "td" => RegionRole::Cell,
            "formula" => RegionRole::Formula,
            "form-field" | "form" => RegionRole::FormField,
            "background" | "artifact" => RegionRole::Background,
            _ => return None,
        })
    }

    /// The structure type of the element made (the outer one for composite roles).
    fn tag(self) -> &'static str {
        match self {
            RegionRole::Text => "P",
            RegionRole::Heading(1) => "H1",
            RegionRole::Heading(2) => "H2",
            RegionRole::Heading(3) => "H3",
            RegionRole::Heading(4) => "H4",
            RegionRole::Heading(5) => "H5",
            RegionRole::Heading(_) => "H6",
            RegionRole::Figure | RegionRole::FigureCaption => "Figure",
            RegionRole::Table => "Table",
            RegionRole::Cell => "TD",
            RegionRole::Formula => "Formula",
            RegionRole::FormField => "Form",
            RegionRole::Background => "Artifact",
        }
    }

    fn takes_alt(self) -> bool {
        matches!(self, RegionRole::Figure | RegionRole::FigureCaption | RegionRole::Formula | RegionRole::FormField)
    }
}

/// Tag a region of a page (see the module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct RegionTag {
    /// 0-based page.
    pub page: usize,
    /// `[x0, y0, x1, y1]` in points from the top-left of the page as displayed (y down).
    pub rect: [f64; 4],
    pub role: RegionRole,
    /// Alternate text for a figure, formula or form field.
    pub alt: Option<String>,
    /// The artifact's /Type (Background only).
    pub artifact_type: Option<ArtifactType>,
    /// The artifact's /Subtype (Background only; implies Pagination).
    pub artifact_subtype: Option<ArtifactSubtype>,
}

/// What [`tag_region`] did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RegionResult {
    /// The elements made, the outer one first (none for Background).
    pub elements: Vec<ObjRef>,
    /// The MCIDs the new elements hold, in content order.
    pub mcids: Vec<i64>,
    /// Painting operations inside the rectangle.
    pub units: usize,
    /// Marked-content sequences that changed as a whole.
    pub moved: usize,
    /// Marked-content sequences split at the rectangle's edge.
    pub split: usize,
    /// Untagged painting operations newly wrapped.
    pub wrapped: usize,
    /// Painting operations that became artifacts.
    pub artifacts: usize,
    /// Elements that left the tree because they were left empty.
    pub removed: usize,
    /// Widgets tagged (Form field).
    pub widgets: usize,
    /// The document had no structure tree and got one.
    pub created_tree: bool,
}

fn invalid(s: impl Into<String>) -> TagError {
    TagError::Invalid(s.into())
}

// ---- Units ---------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Text,
    Image,
    Form,
    Path,
    Shading,
}

#[derive(Clone, Debug)]
struct Unit {
    kind: Kind,
    /// Its operations `start..=end`.
    start: usize,
    end: usize,
    /// Its box in user space (not for text: that comes from the glyphs).
    user: Option<[f64; 4]>,
    /// The innermost tagged (MCID) or artifact sequence around it.
    label: Option<usize>,
}

/// One marked-content sequence.
#[derive(Clone, Debug)]
struct Seq {
    open: usize,
    close: Option<usize>,
    mcid: Option<i64>,
    artifact: bool,
    tag: Vec<u8>,
    /// Its properties (inline, or the page's named /Properties resource).
    props: Option<Dict>,
    /// It is inside a tagged or artifact sequence.
    labelled_parent: bool,
    /// A tagged or artifact sequence is inside it.
    labelled_child: bool,
    /// It paints something that isn't a whole unit (a path broken by a structural operator).
    stray: bool,
}

impl Seq {
    fn labelled(&self) -> bool {
        self.mcid.is_some() || self.artifact
    }
}

struct Scan {
    ops: Vec<Op>,
    skipped: usize,
    units: Vec<Unit>,
    unit_of: Vec<Option<usize>>,
    seqs: Vec<Seq>,
    /// The sequence each BMC/BDC opens.
    opens: HashMap<usize, usize>,
    max_mcid: Option<i64>,
}

fn is_structural(op: &[u8]) -> bool {
    matches!(op, b"q" | b"Q" | b"BT" | b"ET" | b"BMC" | b"BDC" | b"EMC" | b"BX" | b"EX" | b"MP" | b"DP")
}

fn is_path_build(op: &[u8]) -> bool {
    matches!(op, b"m" | b"l" | b"c" | b"v" | b"y" | b"h" | b"re" | b"W" | b"W*")
}

fn is_path_paint(op: &[u8]) -> bool {
    matches!(op, b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" | b"S" | b"s")
}

/// Does the operator paint (or build a path)? A run of new marked content closes before one
/// that isn't part of it.
fn is_content(op: &[u8]) -> bool {
    matches!(op, b"Tj" | b"TJ" | b"'" | b"\"" | b"Do" | b"BI" | b"sh" | b"n") || is_path_build(op) || is_path_paint(op)
}

fn union(a: Option<[f64; 4]>, b: [f64; 4]) -> [f64; 4] {
    match a {
        Some(a) => [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])],
        None => b,
    }
}

fn finite(r: [f64; 4]) -> Option<[f64; 4]> {
    r.iter().all(|v| v.is_finite()).then_some(r)
}

/// The marked-content properties of a BDC: inline or from the page's /Properties.
fn mc_props(doc: &Document, op: &Op, props: &Dict) -> Option<Dict> {
    if op.op.as_slice() != b"BDC" {
        return None;
    }
    match op.operands.get(1) {
        Some(Object::Dict(d)) => Some(d.clone()),
        Some(Object::Name(n)) => props.get(n).map(|x| doc.resolve(x)).and_then(|x| x.as_dict().cloned()),
        _ => None,
    }
}

fn scan(doc: &Document, page: &Dict, number: usize) -> Result<Scan, TagError> {
    let parsed = pdfcraft_content::parse(&page_content(doc, page));
    let res = sub_dict(doc, page, b"Resources");
    let xobjects = sub_dict(doc, &res, b"XObject");
    let props = sub_dict(doc, &res, b"Properties");
    let mut gs: Vec<Matrix> = vec![Matrix::IDENTITY];
    let mut stack: Vec<usize> = Vec::new();
    let mut s = Scan {
        ops: Vec::new(),
        skipped: parsed.skipped,
        units: Vec::new(),
        unit_of: vec![None; parsed.ops.len()],
        seqs: Vec::new(),
        opens: HashMap::new(),
        max_mcid: None,
    };
    // The path being built: its first op, its box, and whether a structural op broke it.
    let mut path: Option<(usize, Option<[f64; 4]>, bool)> = None;
    for (i, op) in parsed.ops.iter().enumerate() {
        let top = gs.last().copied().unwrap_or(Matrix::IDENTITY);
        let name = op.op.as_slice();
        if let Some(p) = path.as_mut()
            && is_structural(name)
        {
            p.2 = true;
        }
        let mut unit: Option<(Kind, usize, Option<[f64; 4]>)> = None;
        match name {
            b"q" => {
                if gs.len() < MAX_GSTATE {
                    gs.push(top);
                }
            }
            b"Q" => {
                if gs.len() > 1 {
                    gs.pop();
                }
            }
            b"cm" => {
                if let Some(m) = Matrix::from_operands(&op.operands)
                    && let Some(t) = gs.last_mut()
                {
                    *t = m.then(&top);
                }
            }
            b"BMC" | b"BDC" => {
                if stack.len() >= MAX_NESTING {
                    return Err(invalid(format!("page {number} nests marked content too deeply to change")));
                }
                let p = mc_props(doc, op, &props);
                let mcid = p.as_ref().and_then(|d| d.get(b"MCID").and_then(Object::as_int)).filter(|m| (0..MAX_MCID).contains(m));
                if let Some(m) = mcid {
                    s.max_mcid = Some(s.max_mcid.map_or(m, |x| x.max(m)));
                }
                let artifact = op.name(0) == Some(b"Artifact");
                let labelled = mcid.is_some() || artifact;
                let labelled_parent = stack.iter().any(|k| s.seqs.get(*k).is_some_and(Seq::labelled));
                if labelled && labelled_parent {
                    for k in &stack {
                        if let Some(q) = s.seqs.get_mut(*k)
                            && q.labelled()
                        {
                            q.labelled_child = true;
                        }
                    }
                }
                s.opens.insert(i, s.seqs.len());
                stack.push(s.seqs.len());
                s.seqs.push(Seq {
                    open: i,
                    close: None,
                    mcid,
                    artifact,
                    tag: op.name(0).map(<[u8]>::to_vec).unwrap_or_else(|| b"Span".to_vec()),
                    props: p,
                    labelled_parent,
                    labelled_child: false,
                    stray: false,
                });
            }
            b"EMC" => {
                if let Some(k) = stack.pop()
                    && let Some(q) = s.seqs.get_mut(k)
                {
                    q.close = Some(i);
                }
            }
            b"Tj" | b"TJ" | b"'" | b"\"" => unit = Some((Kind::Text, i, None)),
            b"Do" => {
                let target = op.name(0).and_then(|n| xobjects.get(n)).map(|x| doc.resolve(x));
                if let Some(Object::Stream(st)) = target.as_deref() {
                    match st.dict.name(b"Subtype") {
                        Some(b"Image") => unit = Some((Kind::Image, i, Some(top.bbox([0.0, 0.0, 1.0, 1.0])))),
                        Some(b"Form") => {
                            let bb: Vec<f64> = st
                                .dict
                                .get(b"BBox")
                                .map(|b| doc.resolve(b))
                                .and_then(|b| b.as_array().map(|a| a.iter().filter_map(Object::as_f64).collect()))
                                .unwrap_or_default();
                            let m = Matrix::from_operands(st.dict.get(b"Matrix").and_then(|m| m.as_array()).map(Vec::as_slice).unwrap_or(&[]))
                                .unwrap_or(Matrix::IDENTITY);
                            let r = match bb[..] {
                                [a, b, c, d] => Some(m.then(&top).bbox([a.min(c), b.min(d), a.max(c), b.max(d)])),
                                _ => None,
                            };
                            unit = Some((Kind::Form, i, r));
                        }
                        _ => {}
                    }
                }
            }
            b"BI" => unit = Some((Kind::Image, i, Some(top.bbox([0.0, 0.0, 1.0, 1.0])))),
            b"sh" => unit = Some((Kind::Shading, i, None)),
            b"m" | b"l" | b"c" | b"v" | b"y" | b"re" | b"h" | b"W" | b"W*" => {
                let entry = path.get_or_insert((i, None, false));
                let mut add = |x: f64, y: f64| {
                    let (u, v) = top.apply(x, y);
                    entry.1 = Some(union(entry.1, [u, v, u, v]));
                };
                match name {
                    b"re" => {
                        if let Some([x, y, w, h]) = op.nums::<4>() {
                            add(x, y);
                            add(x + w, y + h);
                        }
                    }
                    b"c" => {
                        if let Some([x1, y1, x2, y2, x3, y3]) = op.nums::<6>() {
                            add(x1, y1);
                            add(x2, y2);
                            add(x3, y3);
                        }
                    }
                    b"v" | b"y" => {
                        if let Some([x1, y1, x2, y2]) = op.nums::<4>() {
                            add(x1, y1);
                            add(x2, y2);
                        }
                    }
                    b"m" | b"l" => {
                        if let Some([x, y]) = op.nums::<2>() {
                            add(x, y);
                        }
                    }
                    _ => {}
                }
            }
            b"n" => path = None,
            _ if is_path_paint(name) => match path.take() {
                Some((start, b, false)) => unit = Some((Kind::Path, start, b)),
                // Painting across a structural operator can't be wrapped as a whole: it still
                // counts for the marked content around it.
                Some((_, _, true)) => {
                    for k in &stack {
                        if let Some(q) = s.seqs.get_mut(*k) {
                            q.stray = true;
                        }
                    }
                }
                None => unit = Some((Kind::Path, i, None)),
            },
            _ => {}
        }
        if let Some((kind, start, user)) = unit {
            if s.units.len() >= MAX_REGION_UNITS {
                return Err(invalid(format!("page {number} has more than {MAX_REGION_UNITS} painting operations")));
            }
            let label = stack.iter().rev().copied().find(|k| s.seqs.get(*k).is_some_and(Seq::labelled));
            let id = s.units.len();
            for slot in s.unit_of.iter_mut().take(i.saturating_add(1)).skip(start) {
                *slot = Some(id);
            }
            s.units.push(Unit { kind, start, end: i, user: user.and_then(finite), label });
        }
    }
    s.ops = parsed.ops;
    Ok(s)
}

/// The provisional content: each text unit in its own `/Span <</MCID unit>>` sequence (the
/// innermost MCID is the one the text extractor reports).
fn provisional_ops(s: &Scan) -> Vec<Op> {
    let mut out = Vec::with_capacity(s.ops.len().saturating_add(s.units.len().saturating_mul(2)));
    for (i, op) in s.ops.iter().enumerate() {
        let unit = s.unit_of.get(i).copied().flatten().and_then(|u| s.units.get(u).map(|x| (u, x))).filter(|(_, x)| x.kind == Kind::Text);
        if let Some((u, x)) = unit
            && x.start == i
        {
            let mut d = Dict::new();
            d.set(b"MCID".to_vec(), Object::Int(i64::try_from(u).unwrap_or(i64::MAX)));
            out.push(Op::new("BDC", vec![Object::name("Span"), Object::Dict(d)]));
        }
        out.push(op.clone());
        if let Some((_, x)) = unit
            && x.end == i
        {
            out.push(Op::new("EMC", Vec::new()));
        }
    }
    out
}

/// Every unit's box in view space: text from its glyphs, the rest from its geometry.
fn unit_boxes(
    doc: &Document,
    page: usize,
    obj: ObjRef,
    s: &Scan,
    view: &Matrix,
    text: &mut AutotagText<'_>,
) -> Result<Vec<Option<[f64; 4]>>, TagError> {
    let mut boxes: Vec<Option<[f64; 4]>> = s.units.iter().map(|u| u.user.map(|b| view.bbox(b)).and_then(finite)).collect();
    if s.units.iter().any(|u| u.kind == Kind::Text) {
        let mut provisional = doc.clone();
        let st = provisional.add(Object::Stream(Stream::flate(Dict::new(), &pdfcraft_content::serialize_ops(&provisional_ops(s)))));
        provisional.update_dict(obj, |d| d.set(b"Contents".to_vec(), Object::Ref(st)))?;
        let glyphs: Vec<Option<Vec<AutotagGlyph>>> = text(&provisional, &[page]);
        let Some(Some(glyphs)) = glyphs.into_iter().next() else {
            return Err(invalid(format!("the text of page {} could not be read", page.saturating_add(1))));
        };
        for g in &glyphs {
            let Some(r) = finite(g.rect) else { continue };
            let Some(u) = usize::try_from(g.mcid).ok() else { continue };
            if s.units.get(u).is_some_and(|x| x.kind == Kind::Text)
                && let Some(b) = boxes.get_mut(u)
            {
                let r = [r[0].min(r[2]), r[1].min(r[3]), r[0].max(r[2]), r[1].max(r[3])];
                *b = Some(union(*b, r));
            }
        }
    }
    Ok(boxes)
}

fn center_in(rect: [f64; 4], b: [f64; 4]) -> bool {
    let (x, y) = ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0);
    x >= rect[0] - EPS && x <= rect[2] + EPS && y >= rect[1] - EPS && y <= rect[3] + EPS
}

fn normalize(r: [f64; 4]) -> Result<[f64; 4], TagError> {
    if !r.iter().all(|v| v.is_finite()) {
        return Err(invalid("rect must be four finite numbers"));
    }
    let r = [r[0].min(r[2]), r[1].min(r[3]), r[0].max(r[2]), r[1].max(r[3])];
    if r[2] - r[0] <= 0.0 || r[3] - r[1] <= 0.0 {
        return Err(invalid("rect must have a width and a height"));
    }
    Ok(r)
}

// ---- The structure tree --------------------------------------------------------------------

/// The tree root, made (with a `Document` element, the parent tree and `/MarkInfo`) for an
/// untagged document. Returns the root and whether it was made.
fn ensure_tree(doc: &mut Document) -> Result<(ObjRef, bool), TagError> {
    let (cat_ref, cat) = catalog(doc).ok_or_else(|| invalid("the document has no catalog"))?;
    if cat.contains(b"StructTreeRoot") {
        return Ok((root_ref(doc)?, false));
    }
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
    let marked = cat.get(b"MarkInfo").map(|o| doc.resolve(o)).and_then(|o| o.as_dict().cloned());
    doc.update_dict(cat_ref, |c| {
        c.set(b"StructTreeRoot".to_vec(), Object::Ref(r));
        let mut m = marked.unwrap_or_default();
        m.set(b"Marked".to_vec(), Object::Bool(true));
        c.set(b"MarkInfo".to_vec(), Object::Dict(m));
    })?;
    Ok((r, true))
}

fn element(ty: &str, parent: ObjRef, page: ObjRef, kids: Object) -> Object {
    let mut d = Dict::new();
    d.set(b"Type".to_vec(), Object::name("StructElem"));
    d.set(b"S".to_vec(), Object::name(ty));
    d.set(b"P".to_vec(), Object::Ref(parent));
    d.set(b"Pg".to_vec(), Object::Ref(page));
    d.set(b"K".to_vec(), kids);
    Object::Dict(d)
}

fn mcid_list(m: &[i64]) -> Object {
    Object::Array(m.iter().map(|x| Object::Int(*x)).collect())
}

/// Element `e`'s kid MCID `m` on `page_obj` becomes `with` (in its place), as plain MCIDs when
/// the element is on that page and as marked-content references otherwise.
fn replace_mcid(doc: &mut Document, e: ObjRef, here: bool, page_obj: ObjRef, m: i64, with: &[i64]) -> Result<bool, TagError> {
    let Some(d) = doc.get(e).as_dict().cloned() else { return Ok(false) };
    let here = d.get(b"Pg").and_then(Object::as_ref).map_or(here, |p| p == page_obj);
    let kids = kid_list(doc, &d);
    let Some(at) = kids.iter().position(|k| mcr_mcid(doc, k, here, page_obj) == Some(m)) else { return Ok(false) };
    let mut out = Vec::with_capacity(kids.len().saturating_add(with.len()));
    for (i, k) in kids.into_iter().enumerate() {
        if i != at {
            out.push(k);
            continue;
        }
        for x in with {
            if here {
                out.push(Object::Int(*x));
            } else {
                let mut r = Dict::new();
                r.set(b"Type".to_vec(), Object::name("MCR"));
                r.set(b"Pg".to_vec(), Object::Ref(page_obj));
                r.set(b"MCID".to_vec(), Object::Int(*x));
                out.push(Object::Dict(r));
            }
        }
    }
    doc.update_dict(e, |d| d.set(b"K".to_vec(), Object::Array(out)))?;
    Ok(true)
}

/// Remove from element `e` the MCIDs in `gone` on `page_obj` and the object references to
/// `objs`. Returns (removed, now empty).
fn strip(
    doc: &mut Document,
    e: ObjRef,
    here: bool,
    page_obj: ObjRef,
    gone: &HashSet<i64>,
    objs: &HashSet<ObjRef>,
) -> Result<(usize, bool), TagError> {
    let Some(d) = doc.get(e).as_dict().cloned() else { return Ok((0, false)) };
    let here = d.get(b"Pg").and_then(Object::as_ref).map_or(here, |p| p == page_obj);
    let kids = kid_list(doc, &d);
    let before = kids.len();
    let objr = |k: &Object| -> Option<ObjRef> {
        let resolved;
        let d = match k {
            Object::Dict(d) => d,
            Object::Ref(r) => {
                resolved = doc.get(*r);
                resolved.as_dict()?
            }
            _ => return None,
        };
        if d.name(b"Type") != Some(b"OBJR") {
            return None;
        }
        d.get(b"Obj").and_then(Object::as_ref)
    };
    let keep: Vec<Object> = kids
        .into_iter()
        .filter(|k| !mcr_mcid(doc, k, here, page_obj).is_some_and(|m| gone.contains(&m)) && !objr(k).is_some_and(|o| objs.contains(&o)))
        .collect();
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

/// (page, MCID) → the node holding it, for one page.
fn owners(tree: &Tags, page: usize) -> HashMap<i64, usize> {
    let mut out = HashMap::new();
    for (i, n) in tree.nodes.iter().enumerate() {
        for k in &n.kids {
            if let TagKid::Content { page: Some(p), mcid, stream: None } = k
                && *p == page
            {
                out.entry(*mcid).or_insert(i);
            }
        }
    }
    out
}

/// The largest MCID the tree uses on a page.
fn tree_max_mcid(tree: &Tags, page: usize) -> Option<i64> {
    tree.nodes
        .iter()
        .flat_map(|n| n.kids.iter())
        .filter_map(|k| match k {
            TagKid::Content { page: Some(p), mcid, stream: None } if *p == page => Some(*mcid),
            _ => None,
        })
        .max()
}

/// The parent of a node as an object (`Some(None)`: the root), `None` for a direct parent.
fn parent_obj(tree: &Tags, i: usize) -> Option<Option<ObjRef>> {
    match tree.nodes.get(i)?.parent {
        None => Some(None),
        Some(p) => tree.nodes.get(p)?.obj.map(Some),
    }
}

// ---- Tag a region --------------------------------------------------------------------------

/// Where a unit's content goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Target {
    /// Into the new leaf element `k`.
    Leaf(usize),
    /// Into an artifact.
    Artifact,
    /// Stays with the sequence `s` it was in (which is split).
    Old(usize),
}

/// One element to make for the region's content.
struct Leaf {
    tag: &'static str,
    /// Its MCIDs, in content order.
    mcids: Vec<i64>,
    /// Where its first sequence starts in the new content.
    first: Option<usize>,
    /// Table cells: (row, column).
    cell: Option<(usize, usize)>,
}

/// A table row being gathered: its box and its units with their boxes.
type Row = ([f64; 4], Vec<(usize, [f64; 4])>);

/// Group the units of a table region into rows (top to bottom) and cells (left to right).
fn table_cells(units: &[usize], boxes: &[Option<[f64; 4]>]) -> Vec<(usize, Vec<usize>)> {
    let mut items: Vec<(usize, [f64; 4])> = units.iter().filter_map(|u| boxes.get(*u).copied().flatten().map(|b| (*u, b))).collect();
    items.sort_by(|a, b| (a.1[1] + a.1[3]).total_cmp(&(b.1[1] + b.1[3])));
    // Rows: a unit whose centre lies below the row's box starts a new row.
    let mut rows: Vec<Row> = Vec::new();
    for (u, b) in items {
        let cy = (b[1] + b[3]) / 2.0;
        match rows.last_mut() {
            Some((r, list)) if cy <= r[3] => {
                *r = union(Some(*r), b);
                list.push((u, b));
            }
            _ => rows.push((b, vec![(u, b)])),
        }
    }
    let mut out: Vec<(usize, Vec<usize>)> = Vec::new();
    for (row, (rb, mut list)) in rows.into_iter().enumerate() {
        list.sort_by(|a, b| a.1[0].total_cmp(&b.1[0]));
        let gap = ((rb[3] - rb[1]) * 0.8).max(2.0);
        let mut right = f64::NEG_INFINITY;
        for (u, b) in list {
            match out.last_mut() {
                Some((r, cell)) if *r == row && b[0] - right <= gap => cell.push(u),
                _ => out.push((row, vec![u])),
            }
            right = right.max(b[2]);
        }
    }
    out
}

/// Tag what lies inside a rectangle (see the module docs); all or nothing. `text` reads a
/// provisional copy's glyphs (as for [`crate::autotag`]).
pub fn tag_region(doc: &mut Document, req: &RegionTag, text: &mut AutotagText<'_>) -> Result<RegionResult, TagError> {
    let mut work = doc.clone();
    let r = tag_region_inner(&mut work, req, text)?;
    *doc = work;
    Ok(r)
}

fn clean_alt(alt: &Option<String>) -> Result<Option<String>, TagError> {
    match alt.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) if s.chars().count() > MAX_TEXT => Err(invalid(format!("alt is longer than {MAX_TEXT} characters"))),
        Some(s) => Ok(Some(s.to_owned())),
        None => Ok(None),
    }
}

fn tag_region_inner(doc: &mut Document, req: &RegionTag, text: &mut AutotagText<'_>) -> Result<RegionResult, TagError> {
    let rect = normalize(req.rect)?;
    let alt = clean_alt(&req.alt)?;
    if alt.is_some() && !req.role.takes_alt() {
        return Err(invalid(format!("alt is for figure, figure-caption, formula and form-field regions, not {}", req.role.id())));
    }
    if (req.artifact_type.is_some() || req.artifact_subtype.is_some()) && req.role != RegionRole::Background {
        return Err(invalid("artifact_type and artifact_subtype are for background regions"));
    }
    let art_type = match (req.artifact_type, req.artifact_subtype) {
        (Some(t), Some(_)) if t != ArtifactType::Pagination => {
            return Err(invalid("only pagination artifacts have a subtype (Header, Footer or Watermark); use type Pagination"));
        }
        (None, Some(_)) => Some(ArtifactType::Pagination),
        (t, _) => t,
    };
    let pages = pdfcraft_model::pages(doc);
    let pg = pages
        .get(req.page)
        .ok_or_else(|| invalid(format!("there is no page {}: the document has {} pages", req.page.saturating_add(1), pages.len())))?;
    let (page_obj, number) = (pg.obj, req.page.saturating_add(1));
    let tree = tags(doc);
    if tree.exists && tree.truncated {
        return Err(invalid("the structure tree is too large or too deeply nested to edit safely"));
    }
    let (view, _) = view_of(doc, pg);
    if req.role == RegionRole::FormField {
        return tag_widgets(doc, req.page, pg, &view, rect, alt);
    }

    let s = scan(doc, &pg.dict, number)?;
    let boxes = unit_boxes(doc, req.page, page_obj, &s, &view, text)?;
    let selected: Vec<usize> = (0..s.units.len()).filter(|u| boxes.get(*u).copied().flatten().is_some_and(|b| center_in(rect, b))).collect();
    if selected.is_empty() {
        return Err(invalid(format!(
            "nothing on page {number} lies inside the rectangle (a painting operation is inside when the centre of its box is)"
        )));
    }
    if s.skipped > 0 {
        return Err(invalid(format!("page {number}'s content has bytes that can't be read; it is left unchanged so nothing is lost")));
    }

    // The elements to make, and where each selected unit goes.
    let mut leaves: Vec<Leaf> = Vec::new();
    let leaf = |tag: &'static str| Leaf { tag, mcids: Vec::new(), first: None, cell: None };
    let mut target: Vec<Option<Target>> = vec![None; s.units.len()];
    let kind = |u: usize| s.units.get(u).map(|x| x.kind);
    match req.role {
        RegionRole::Background => {
            for &u in &selected {
                if let Some(t) = target.get_mut(u) {
                    *t = Some(Target::Artifact);
                }
            }
        }
        RegionRole::FigureCaption => {
            let (texts, pictures): (Vec<usize>, Vec<usize>) = selected.iter().partition(|u| kind(**u) == Some(Kind::Text));
            if pictures.is_empty() {
                return Err(invalid(format!(
                    "Figure/Caption needs a picture or drawing inside the rectangle on page {number}; tag text alone as text or a heading"
                )));
            }
            leaves.push(leaf("Figure"));
            for u in pictures {
                if let Some(t) = target.get_mut(u) {
                    *t = Some(Target::Leaf(0));
                }
            }
            if !texts.is_empty() {
                leaves.push(leaf("Caption"));
                for u in texts {
                    if let Some(t) = target.get_mut(u) {
                        *t = Some(Target::Leaf(1));
                    }
                }
            }
        }
        RegionRole::Table => {
            let (drawings, content): (Vec<usize>, Vec<usize>) =
                selected.iter().partition(|u| matches!(kind(**u), Some(Kind::Path) | Some(Kind::Shading)));
            if content.is_empty() {
                return Err(invalid(format!("there is no text or picture inside the rectangle on page {number} to make table cells of")));
            }
            for u in drawings {
                if let Some(t) = target.get_mut(u) {
                    *t = Some(Target::Artifact);
                }
            }
            let cells = table_cells(&content, &boxes);
            if cells.len() > MAX_TABLE_CELLS {
                return Err(invalid(format!("a table region makes at most {MAX_TABLE_CELLS} cells")));
            }
            let mut col = 0usize;
            let mut last_row = usize::MAX;
            for (row, units) in cells {
                col = if row == last_row { col.saturating_add(1) } else { 0 };
                last_row = row;
                let k = leaves.len();
                leaves.push(Leaf { tag: if row == 0 { "TH" } else { "TD" }, mcids: Vec::new(), first: None, cell: Some((row, col)) });
                for u in units {
                    if let Some(t) = target.get_mut(u) {
                        *t = Some(Target::Leaf(k));
                    }
                }
            }
        }
        role => {
            leaves.push(leaf(role.tag()));
            for &u in &selected {
                if let Some(t) = target.get_mut(u) {
                    *t = Some(Target::Leaf(0));
                }
            }
        }
    }

    // Each tagged or artifact sequence with selected units changes as a whole or is split.
    let mut by_seq: BTreeMap<usize, (HashSet<Target>, usize)> = BTreeMap::new();
    for (u, unit) in s.units.iter().enumerate() {
        let Some(l) = unit.label else { continue };
        let e = by_seq.entry(l).or_default();
        match target.get(u).copied().flatten() {
            Some(t) => {
                e.0.insert(t);
            }
            None => e.1 = e.1.saturating_add(1),
        }
    }
    let mut convert: HashMap<usize, Target> = HashMap::new();
    let mut dissolve: BTreeSet<usize> = BTreeSet::new();
    for (l, (targets, outside)) in &by_seq {
        if targets.is_empty() {
            continue;
        }
        let Some(q) = s.seqs.get(*l) else { continue };
        if q.labelled_parent || q.labelled_child {
            return Err(invalid(format!(
                "page {number} has tagged or artifact marked content nested inside other tagged content in the rectangle; it can't be retagged safely"
            )));
        }
        if q.close.is_none() {
            return Err(invalid(format!("marked content on page {number} that the rectangle covers is not closed; it can't be retagged")));
        }
        match (targets.iter().next(), targets.len(), *outside) {
            (Some(t), 1, 0) if !q.stray => {
                convert.insert(*l, *t);
            }
            _ if q.stray => {
                return Err(invalid(format!(
                    "marked content on page {number} that the rectangle cuts through paints a path across other operators; it can't be split"
                )));
            }
            _ => {
                dissolve.insert(*l);
            }
        }
    }
    for (u, unit) in s.units.iter().enumerate() {
        if let (Some(l), Some(t)) = (unit.label, target.get_mut(u))
            && t.is_none()
            && dissolve.contains(&l)
        {
            *t = Some(Target::Old(l));
        }
        // A unit inside a sequence that changes as a whole goes with it: its own wrapper isn't
        // needed.
        if let (Some(l), Some(t)) = (unit.label, target.get_mut(u))
            && convert.contains_key(&l)
        {
            *t = None;
        }
    }
    let skip: HashSet<usize> = dissolve.iter().filter_map(|l| s.seqs.get(*l)).flat_map(|q| [Some(q.open), q.close]).flatten().collect();
    let converts_at: HashMap<usize, (usize, Target)> = convert.iter().filter_map(|(l, t)| s.seqs.get(*l).map(|q| (q.open, (*l, *t)))).collect();

    // The tree's elements for the touched MCIDs.
    let owner = owners(&tree, req.page);
    for l in convert.keys().chain(dissolve.iter()) {
        let Some(m) = s.seqs.get(*l).and_then(|q| q.mcid) else { continue };
        if let Some(n) = owner.get(&m).and_then(|i| tree.nodes.get(*i))
            && n.obj.is_none()
        {
            return Err(invalid(format!(
                "MCID {m} on page {number} belongs to an element written inside its parent, which can't be edited; retag it from the Tags panel"
            )));
        }
    }
    let mut next = s.max_mcid.into_iter().chain(tree_max_mcid(&tree, req.page)).max().map_or(0, |m| m.saturating_add(1));
    let mut take = || -> Result<i64, TagError> {
        if !(0..MAX_MCID).contains(&next) {
            return Err(invalid(format!("page {number} has no free marked-content ids")));
        }
        let m = next;
        next = next.saturating_add(1);
        Ok(m)
    };
    let art_props = art_type.map(|t| {
        let mut d = Dict::new();
        d.set(b"Type".to_vec(), Object::name(t.name()));
        if let Some(st) = req.artifact_subtype {
            d.set(b"Subtype".to_vec(), Object::name(st.name()));
        }
        d
    });
    let artifact_open = |props: &Option<Dict>| match props {
        Some(d) => Op::new("BDC", vec![Object::name("Artifact"), Object::Dict(d.clone())]),
        None => Op::new("BMC", vec![Object::name("Artifact")]),
    };
    // Table rules become layout artifacts.
    let rule_props = (req.role == RegionRole::Table).then(|| {
        let mut d = Dict::new();
        d.set(b"Type".to_vec(), Object::name("Layout"));
        d
    });
    let new_art_props = if req.role == RegionRole::Table { rule_props } else { art_props };
    let mut result = RegionResult { units: selected.len(), moved: convert.len(), split: dissolve.len(), ..Default::default() };

    // The new content.
    let mut out: Vec<Op> = Vec::with_capacity(s.ops.len().saturating_add(s.units.len().saturating_mul(2)));
    let mut open: Option<Target> = None;
    let mut reused: HashSet<usize> = HashSet::new();
    // The MCIDs each split sequence's outside runs get, in order.
    let mut old_runs: BTreeMap<usize, Vec<i64>> = BTreeMap::new();
    // MCIDs whose content left their element.
    let mut gone: HashSet<i64> = HashSet::new();
    let mut counted: HashSet<usize> = HashSet::new();
    for (i, op) in s.ops.iter().enumerate() {
        if skip.contains(&i) {
            continue;
        }
        let unit = s.unit_of.get(i).copied().flatten();
        let t = unit.and_then(|u| target.get(u).copied().flatten());
        if let Some(t) = t {
            if open != Some(t) {
                if open.is_some() {
                    out.push(Op::new("EMC", Vec::new()));
                }
                let opening = match t {
                    Target::Leaf(k) => {
                        let m = take()?;
                        let lf = leaves.get_mut(k).ok_or_else(|| invalid("internal: a leaf is missing"))?;
                        lf.mcids.push(m);
                        lf.first.get_or_insert(out.len());
                        let mut d = Dict::new();
                        d.set(b"MCID".to_vec(), Object::Int(m));
                        Op::new("BDC", vec![Object::name(lf.tag), Object::Dict(d)])
                    }
                    Target::Artifact => artifact_open(&new_art_props),
                    Target::Old(l) => {
                        let q = s.seqs.get(l).ok_or_else(|| invalid("internal: a sequence is missing"))?;
                        let mut d = q.props.clone().unwrap_or_default();
                        for key in [&b"MCID"[..], b"ActualText", b"Alt", b"E"] {
                            d.remove(key);
                        }
                        if q.mcid.is_some() {
                            let m = match q.mcid {
                                Some(m) if reused.insert(l) => m,
                                _ => take()?,
                            };
                            old_runs.entry(l).or_default().push(m);
                            d.set(b"MCID".to_vec(), Object::Int(m));
                            Op::new("BDC", vec![Object::Name(q.tag.clone()), Object::Dict(d)])
                        } else if d.is_empty() {
                            Op::new("BMC", vec![Object::name("Artifact")])
                        } else {
                            Op::new("BDC", vec![Object::name("Artifact"), Object::Dict(d)])
                        }
                    }
                };
                out.push(opening);
                open = Some(t);
            }
            if let Some(u) = unit
                && counted.insert(u)
            {
                match (t, s.units.get(u).and_then(|x| x.label)) {
                    (Target::Artifact, _) => result.artifacts = result.artifacts.saturating_add(1),
                    (Target::Leaf(_), None) => result.wrapped = result.wrapped.saturating_add(1),
                    _ => {}
                }
            }
            out.push(op.clone());
            continue;
        }
        let name = op.op.as_slice();
        if open.is_some() && (is_structural(name) || is_content(name)) {
            out.push(Op::new("EMC", Vec::new()));
            open = None;
        }
        if let Some((l, t)) = converts_at.get(&i) {
            let q = s.seqs.get(*l).ok_or_else(|| invalid("internal: a sequence is missing"))?;
            if let Some(m) = q.mcid {
                gone.insert(m);
            }
            let painted = s.units.iter().filter(|x| x.label == Some(*l)).count();
            let opening = match t {
                Target::Leaf(k) => {
                    let m = match q.mcid {
                        Some(m) => m,
                        None => take()?,
                    };
                    let lf = leaves.get_mut(*k).ok_or_else(|| invalid("internal: a leaf is missing"))?;
                    lf.mcids.push(m);
                    lf.first.get_or_insert(out.len());
                    let mut d = q.props.clone().unwrap_or_default();
                    for key in [&b"ActualText"[..], b"Alt", b"E", b"Type", b"Subtype", b"BBox", b"Attached"] {
                        d.remove(key);
                    }
                    d.set(b"MCID".to_vec(), Object::Int(m));
                    Op::new("BDC", vec![Object::name(lf.tag), Object::Dict(d)])
                }
                _ => {
                    result.artifacts = result.artifacts.saturating_add(painted);
                    artifact_open(&new_art_props)
                }
            };
            out.push(opening);
            continue;
        }
        out.push(op.clone());
    }
    if open.is_some() {
        out.push(Op::new("EMC", Vec::new()));
    }
    for l in &dissolve {
        let Some(m) = s.seqs.get(*l).and_then(|q| q.mcid) else { continue };
        if !old_runs.contains_key(l) {
            gone.insert(m);
        }
    }

    // Write it: the content, then the tree.
    let tagged_role = req.role != RegionRole::Background;
    let has_tree = tree.exists;
    let root = if tagged_role {
        let (root, made) = ensure_tree(doc)?;
        result.created_tree = made;
        Some(root)
    } else if has_tree {
        Some(root_ref(doc)?)
    } else {
        None
    };
    let st = doc.add(Object::Stream(Stream::flate(Dict::new(), &pdfcraft_content::serialize_ops(&out))));
    doc.update_dict(page_obj, |d| d.set(b"Contents".to_vec(), Object::Ref(st)))?;
    let Some(root) = root else { return Ok(result) };

    // Old elements: split MCIDs are replaced in place, moved ones leave.
    let mut emptied: Vec<ObjRef> = Vec::new();
    for (l, runs) in &old_runs {
        let Some(m) = s.seqs.get(*l).and_then(|q| q.mcid) else { continue };
        if let Some(n) = owner.get(&m).and_then(|i| tree.nodes.get(*i))
            && let Some(o) = n.obj
        {
            replace_mcid(doc, o, n.page == Some(req.page), page_obj, m, runs)?;
        }
    }
    let holders: BTreeSet<ObjRef> = gone.iter().filter_map(|m| owner.get(m).and_then(|i| tree.nodes.get(*i)).and_then(|n| n.obj)).collect();
    let none: HashSet<ObjRef> = HashSet::new();
    for e in holders {
        let here = tree.find(e).and_then(|i| tree.nodes.get(i)).and_then(|n| n.page) == Some(req.page);
        let (_, empty) = strip(doc, e, here, page_obj, &gone, &none)?;
        if empty {
            emptied.push(e);
        }
    }
    let parents: HashMap<ObjRef, Option<ObjRef>> =
        tree.nodes.iter().enumerate().filter_map(|(i, n)| n.obj.and_then(|o| parent_obj(&tree, i).map(|p| (o, p)))).collect();
    let parent_of = |e: ObjRef| parents.get(&e).copied();
    result.removed = prune(doc, root, emptied, &parent_of)?;

    // The new elements.
    if tagged_role {
        build_elements(doc, root, req, page_obj, &out, &leaves, alt, &mut result)?;
    }
    rebuild_parent_tree(doc, root, req.page, page_obj)?;
    Ok(result)
}

/// (content position, element) of every tagged sequence in `ops` that the tree reaches.
fn placed_in(tree: &Tags, page: usize, ops: &[Op], props: &Dict, doc: &Document) -> Vec<(usize, ObjRef)> {
    let owner = owners(tree, page);
    ops.iter()
        .enumerate()
        .filter_map(|(i, op)| {
            let m = mc_props(doc, op, props)?.get(b"MCID").and_then(Object::as_int)?;
            let n = tree.nodes.get(*owner.get(&m)?)?;
            Some((i, n.obj?))
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn build_elements(
    doc: &mut Document,
    root: ObjRef,
    req: &RegionTag,
    page_obj: ObjRef,
    out: &[Op],
    leaves: &[Leaf],
    alt: Option<String>,
    result: &mut RegionResult,
) -> Result<(), TagError> {
    let tree = tags(doc);
    let props =
        pdfcraft_model::pages(doc).get(req.page).map(|p| sub_dict(doc, &sub_dict(doc, &p.dict, b"Resources"), b"Properties")).unwrap_or_default();
    let placed = placed_in(&tree, req.page, out, &props, doc);
    let at = leaves.iter().filter_map(|l| l.first).min().unwrap_or(out.len());
    let mut elements = Elements::new(&tree);
    let all: Vec<i64> = {
        let mut v: Vec<(usize, i64)> = Vec::new();
        for l in leaves {
            // Leaf MCIDs are in content order; merge them by where each first appears.
            v.extend(l.mcids.iter().map(|m| (l.first.unwrap_or(usize::MAX), *m)));
        }
        v.sort();
        v.into_iter().map(|(_, m)| m).collect()
    };
    result.mcids = all;
    let set_alt = |doc: &mut Document, e: ObjRef| -> Result<(), TagError> {
        if let Some(a) = &alt {
            doc.update_dict(e, |d| d.set(b"Alt".to_vec(), Object::String(PdfString::text(a))))?;
        }
        Ok(())
    };
    match req.role {
        RegionRole::Table => {
            let table = place_element(doc, root, &tree, &mut elements, &placed, req.page, page_obj, at, "Table", Object::Array(Vec::new()))?;
            result.elements.push(table);
            let mut rows: Vec<(usize, ObjRef, Vec<Object>)> = Vec::new();
            let mut cells: Vec<ObjRef> = Vec::new();
            for l in leaves {
                let Some((row, _)) = l.cell else { continue };
                if rows.last().is_none_or(|r| r.0 != row) {
                    let tr = doc.add(element("TR", table, page_obj, Object::Array(Vec::new())));
                    rows.push((row, tr, Vec::new()));
                }
                let Some(r) = rows.last_mut() else { continue };
                let cell = doc.add(element(l.tag, r.1, page_obj, mcid_list(&l.mcids)));
                if l.tag == "TH" {
                    let mut a = Dict::new();
                    a.set(b"O".to_vec(), Object::name("Table"));
                    a.set(b"Scope".to_vec(), Object::name("Column"));
                    doc.update_dict(cell, |d| d.set(b"A".to_vec(), Object::Dict(a)))?;
                }
                r.2.push(Object::Ref(cell));
                cells.push(cell);
            }
            let mut trs = Vec::with_capacity(rows.len());
            for (_, tr, cells) in rows {
                doc.update_dict(tr, |d| d.set(b"K".to_vec(), Object::Array(cells)))?;
                trs.push(Object::Ref(tr));
                result.elements.push(tr);
            }
            result.elements.extend(cells);
            doc.update_dict(table, |d| d.set(b"K".to_vec(), Object::Array(trs)))?;
        }
        RegionRole::FigureCaption => {
            let fig = leaves.first().ok_or_else(|| invalid("internal: no figure"))?;
            let figure = place_element(doc, root, &tree, &mut elements, &placed, req.page, page_obj, at, "Figure", mcid_list(&fig.mcids))?;
            set_alt(doc, figure)?;
            result.elements.push(figure);
            if let Some(cap) = leaves.get(1) {
                let caption = doc.add(element("Caption", figure, page_obj, mcid_list(&cap.mcids)));
                let mut kids: Vec<Object> = fig.mcids.iter().map(|m| Object::Int(*m)).collect();
                // The caption goes first when it comes first in the content, else last.
                if cap.first < fig.first {
                    kids.insert(0, Object::Ref(caption));
                } else {
                    kids.push(Object::Ref(caption));
                }
                doc.update_dict(figure, |d| d.set(b"K".to_vec(), Object::Array(kids)))?;
                result.elements.push(caption);
            }
        }
        RegionRole::Cell => {
            let l = leaves.first().ok_or_else(|| invalid("internal: no cell"))?;
            let kids = mcid_list(&l.mcids);
            // In the row of the nearest cell before it (after it), when there is one.
            let before = placed.iter().filter(|(i, _)| *i < at).max_by_key(|(i, _)| *i);
            let after = placed.iter().filter(|(i, _)| *i > at).min_by_key(|(i, _)| *i);
            let row_cell = |e: ObjRef| -> Option<(ObjRef, ObjRef)> {
                let mut i = tree.find(e)?;
                for _ in 0..64 {
                    let n = tree.nodes.get(i)?;
                    let p = tree.nodes.get(n.parent?)?;
                    if matches!(n.role(), "TD" | "TH") && p.role() == "TR" {
                        return Some((p.obj?, n.obj?));
                    }
                    i = n.parent?;
                }
                None
            };
            let spot =
                before.and_then(|(_, e)| row_cell(*e).map(|x| (x, true))).or_else(|| after.and_then(|(_, e)| row_cell(*e).map(|x| (x, false))));
            let cell = match spot {
                Some(((tr, anchor), after_it)) => {
                    let cell = doc.add(element("TD", tr, page_obj, kids));
                    insert_kid(doc, root, Some(tr), Some(anchor), after_it, cell)?;
                    cell
                }
                None => place_element(doc, root, &tree, &mut elements, &placed, req.page, page_obj, at, "TD", kids)?,
            };
            result.elements.push(cell);
        }
        role => {
            let l = leaves.first().ok_or_else(|| invalid("internal: no element"))?;
            let e = place_element(doc, root, &tree, &mut elements, &placed, req.page, page_obj, at, role.tag(), mcid_list(&l.mcids))?;
            set_alt(doc, e)?;
            result.elements.push(e);
        }
    }
    Ok(())
}

/// Form field: each widget whose box's centre is inside becomes a `Form` element with an
/// object reference, a `/StructParent` and a parent-tree entry; one already tagged moves.
fn tag_widgets(
    doc: &mut Document,
    page: usize,
    pg: &pdfcraft_model::Page,
    view: &Matrix,
    rect: [f64; 4],
    alt: Option<String>,
) -> Result<RegionResult, TagError> {
    let number = page.saturating_add(1);
    let annots = pg.dict.get(b"Annots").map(|a| doc.resolve(a)).and_then(|a| a.as_array().cloned()).unwrap_or_default();
    let mut widgets: Vec<ObjRef> = Vec::new();
    let mut seen = HashSet::new();
    for a in annots.iter().take(MAX_WIDGETS) {
        let Some(r) = a.as_ref() else { continue };
        if !seen.insert(r) {
            continue;
        }
        let Some(d) = doc.get(r).as_dict().cloned() else { continue };
        if d.name(b"Subtype") != Some(b"Widget") {
            continue;
        }
        let nums: Vec<f64> = d
            .get(b"Rect")
            .map(|x| doc.resolve(x))
            .and_then(|x| x.as_array().map(|a| a.iter().filter_map(Object::as_f64).collect()))
            .unwrap_or_default();
        let [a0, a1, a2, a3] = nums[..] else { continue };
        let Some(b) = finite(view.bbox([a0.min(a2), a1.min(a3), a0.max(a2), a1.max(a3)])) else { continue };
        if center_in(rect, b) {
            widgets.push(r);
        }
    }
    if widgets.is_empty() {
        return Err(invalid(format!("no form field widget on page {number} lies inside the rectangle")));
    }
    let (root, created_tree) = ensure_tree(doc)?;
    let mut result = RegionResult { widgets: widgets.len(), created_tree, ..Default::default() };
    // Widgets already tagged leave their elements.
    let tree = tags(doc);
    let set: HashSet<ObjRef> = widgets.iter().copied().collect();
    let mut emptied = Vec::new();
    let none: HashSet<i64> = HashSet::new();
    for n in &tree.nodes {
        if !n.kids.iter().any(|k| matches!(k, TagKid::Object { obj, .. } if set.contains(obj))) {
            continue;
        }
        let Some(o) = n.obj else {
            return Err(invalid(format!("a widget on page {number} belongs to an element written inside its parent, which can't be edited")));
        };
        let (_, empty) = strip(doc, o, n.page == Some(page), pg.obj, &none, &set)?;
        if empty {
            emptied.push(o);
        }
    }
    let parents: HashMap<ObjRef, Option<ObjRef>> =
        tree.nodes.iter().enumerate().filter_map(|(i, n)| n.obj.and_then(|o| parent_obj(&tree, i).map(|p| (o, p)))).collect();
    let parent_of = |e: ObjRef| parents.get(&e).copied();
    result.removed = prune(doc, root, emptied, &parent_of)?;

    // After the page's tagged content.
    let tree = tags(doc);
    let props = sub_dict(doc, &sub_dict(doc, &pg.dict, b"Resources"), b"Properties");
    let ops = pdfcraft_content::parse(&page_content(doc, &pg.dict)).ops;
    let mut placed = placed_in(&tree, page, &ops, &props, doc);
    let mut elements = Elements::new(&tree);
    let pt = parent_tree(doc, root)?;
    for (k, w) in widgets.iter().enumerate() {
        let mut objr = Dict::new();
        objr.set(b"Type".to_vec(), Object::name("OBJR"));
        objr.set(b"Obj".to_vec(), Object::Ref(*w));
        objr.set(b"Pg".to_vec(), Object::Ref(pg.obj));
        let at = ops.len().saturating_add(k);
        let form = place_element(doc, root, &tree, &mut elements, &placed, page, pg.obj, at, "Form", Object::Dict(objr))?;
        placed.push((at, form));
        let wd = doc.get(*w).as_dict().cloned().unwrap_or_default();
        let text = alt.clone().or_else(|| wd.get(b"TU").map(|t| doc.resolve(t)).and_then(|t| t.as_string().map(PdfString::to_text)));
        let text = text.filter(|t| !t.trim().is_empty());
        if let Some(t) = text {
            doc.update_dict(form, |d| d.set(b"Alt".to_vec(), Object::String(PdfString::text(&t))))?;
        }
        let key = match wd.get(b"StructParent").and_then(Object::as_int).filter(|k| *k >= 0) {
            Some(k) => k,
            None => {
                let next = doc.get(root).as_dict().and_then(|d| d.get(b"ParentTreeNextKey").and_then(Object::as_int)).unwrap_or(0);
                number_max(doc, pt).map_or(0, |m| m.saturating_add(1)).max(next).max(0)
            }
        };
        if !put_number(doc, pt, key, Object::Ref(form))? {
            return Err(invalid("the parent tree has no room for the widget's entry"));
        }
        doc.update_dict(*w, |d| d.set(b"StructParent".to_vec(), Object::Int(key)))?;
        let next = doc.get(root).as_dict().and_then(|d| d.get(b"ParentTreeNextKey").and_then(Object::as_int)).unwrap_or(0);
        if next <= key {
            doc.update_dict(root, |d| d.set(b"ParentTreeNextKey".to_vec(), Object::Int(key.saturating_add(1))))?;
        }
        result.elements.push(form);
    }
    Ok(result)
}

// ---- The Order panel -----------------------------------------------------------------------

/// An element that holds content on the page.
#[derive(Clone, Debug, PartialEq)]
pub struct OrderElement {
    /// Its node in [`Tags::nodes`].
    pub node: usize,
    /// `None` for an element written inside its parent (it can't be moved).
    pub obj: Option<ObjRef>,
    pub ty: String,
    pub standard: Option<String>,
    /// Its MCIDs on the page, in order.
    pub mcids: Vec<i64>,
    /// The annotations it references on the page.
    pub objects: Vec<ObjRef>,
    /// Where its content is drawn, in points from the top-left of the displayed page.
    pub rect: Option<[f64; 4]>,
    pub alt: Option<String>,
    pub actual_text: Option<String>,
}

/// One numbered region of the Reading Order tool: the outermost element below a grouping
/// element (Document, Part, Sect, Div …) that holds content on the page.
#[derive(Clone, Debug, PartialEq)]
pub struct OrderRegion {
    pub node: usize,
    pub obj: Option<ObjRef>,
    pub ty: String,
    pub standard: Option<String>,
    pub rect: Option<[f64; 4]>,
    /// The elements inside it (itself included) that hold content on the page.
    pub elements: Vec<OrderElement>,
}

/// A page's reading order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageOrder {
    /// 0-based page.
    pub page: usize,
    /// The document has a structure tree.
    pub tagged: bool,
    /// The regions, in structure (reading) order.
    pub regions: Vec<OrderRegion>,
    /// Painting operations neither tagged nor artifacts.
    pub untagged: usize,
    /// Painting operations in artifacts.
    pub artifacts: usize,
    /// Painting operations in marked content no element reaches.
    pub orphaned: usize,
    /// The tree was too large to read in full.
    pub truncated: bool,
}

/// The page's reading order (see [`PageOrder`]). `text` reads the provisional page's glyphs.
pub fn page_order(doc: &Document, page: usize, text: &mut AutotagText<'_>) -> Result<PageOrder, TagError> {
    let pages = pdfcraft_model::pages(doc);
    let pg =
        pages.get(page).ok_or_else(|| invalid(format!("there is no page {}: the document has {} pages", page.saturating_add(1), pages.len())))?;
    let tree = tags(doc);
    let (view, _) = view_of(doc, pg);
    let s = scan(doc, &pg.dict, page.saturating_add(1))?;
    let boxes = unit_boxes(doc, page, pg.obj, &s, &view, text)?;
    let owner = owners(&tree, page);
    let mut by_mcid: HashMap<i64, [f64; 4]> = HashMap::new();
    let mut out = PageOrder { page, tagged: tree.exists, truncated: tree.truncated, ..Default::default() };
    for (u, unit) in s.units.iter().enumerate() {
        // The innermost MCID around it: an artifact anywhere around it makes it an artifact.
        let q = unit.label.and_then(|l| s.seqs.get(l));
        match q {
            None => out.untagged = out.untagged.saturating_add(1),
            Some(q) if q.artifact && q.mcid.is_none() => out.artifacts = out.artifacts.saturating_add(1),
            Some(q) => {
                if let Some(m) = q.mcid {
                    if !owner.contains_key(&m) {
                        out.orphaned = out.orphaned.saturating_add(1);
                    }
                    if let Some(b) = boxes.get(u).copied().flatten() {
                        let e = by_mcid.get(&m).copied();
                        by_mcid.insert(m, union(e, b));
                    }
                }
            }
        }
    }
    let annot_box = |o: ObjRef| -> Option<[f64; 4]> {
        let d = doc.get(o).as_dict().cloned()?;
        let v: Vec<f64> = d.get(b"Rect").map(|x| doc.resolve(x)).and_then(|x| x.as_array().map(|a| a.iter().filter_map(Object::as_f64).collect()))?;
        let [a, b, c, e] = v[..] else { return None };
        finite(view.bbox([a.min(c), b.min(e), a.max(c), b.max(e)]))
    };
    let page_annots: HashSet<ObjRef> = pg
        .dict
        .get(b"Annots")
        .map(|a| doc.resolve(a))
        .and_then(|a| a.as_array().map(|a| a.iter().filter_map(Object::as_ref).collect()))
        .unwrap_or_default();
    let mut index: HashMap<usize, usize> = HashMap::new();
    for (i, n) in tree.nodes.iter().enumerate() {
        let mcids: Vec<i64> = n
            .kids
            .iter()
            .filter_map(|k| match k {
                TagKid::Content { page: Some(p), mcid, stream: None } if *p == page => Some(*mcid),
                _ => None,
            })
            .collect();
        let objects: Vec<ObjRef> = n
            .kids
            .iter()
            .filter_map(|k| match k {
                TagKid::Object { obj, page: p, .. } if *p == Some(page) || page_annots.contains(obj) => Some(*obj),
                _ => None,
            })
            .collect();
        if mcids.is_empty() && objects.is_empty() {
            continue;
        }
        let mut rect = None;
        for m in &mcids {
            if let Some(b) = by_mcid.get(m) {
                rect = Some(union(rect, *b));
            }
        }
        for o in &objects {
            if let Some(b) = annot_box(*o) {
                rect = Some(union(rect, b));
            }
        }
        // The region: climb while the parent isn't a grouping element.
        let mut r = i;
        if !GROUPING.contains(&n.role()) {
            for _ in 0..crate::keep::MAX_CLIMB {
                let Some(p) = tree.nodes.get(r).and_then(|x| x.parent) else { break };
                if tree.nodes.get(p).is_none_or(|x| GROUPING.contains(&x.role())) {
                    break;
                }
                r = p;
            }
        }
        let el = OrderElement {
            node: i,
            obj: n.obj,
            ty: n.ty.clone(),
            standard: n.standard.clone(),
            mcids,
            objects,
            rect,
            alt: n.alt.clone(),
            actual_text: n.actual_text.clone(),
        };
        let at = match index.get(&r) {
            Some(at) => *at,
            None => {
                let Some(rn) = tree.nodes.get(r) else { continue };
                out.regions.push(OrderRegion {
                    node: r,
                    obj: rn.obj,
                    ty: rn.ty.clone(),
                    standard: rn.standard.clone(),
                    rect: None,
                    elements: Vec::new(),
                });
                index.insert(r, out.regions.len().saturating_sub(1));
                out.regions.len().saturating_sub(1)
            }
        };
        if let Some(region) = out.regions.get_mut(at) {
            if let Some(b) = rect {
                region.rect = Some(union(region.rect, b));
            }
            region.elements.push(el);
        }
    }
    Ok(out)
}

/// Where [`move_element`] puts an element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderPlace {
    /// Just before this element, among its siblings.
    Before(ObjRef),
    /// Just after this element, among its siblings.
    After(ObjRef),
    /// Inside this element, as its last kid (an empty element too).
    Into(ObjRef),
}

/// Order panel: move an element (with everything inside it) to just before or after another,
/// which changes the reading order. The element's `/P` follows; MCIDs and the parent tree are
/// unchanged.
pub fn move_element(doc: &mut Document, element: ObjRef, place: OrderPlace) -> Result<(), TagError> {
    let tree = tags(doc);
    if !tree.exists {
        return Err(TagError::Untagged);
    }
    if tree.truncated {
        return Err(invalid("the structure tree is too large or too deeply nested to edit safely"));
    }
    let root = root_ref(doc)?;
    let (target, after, into) = match place {
        OrderPlace::Before(t) => (t, false, false),
        OrderPlace::After(t) => (t, true, false),
        OrderPlace::Into(t) => (t, true, true),
    };
    if target == element {
        return Err(invalid(if into { "an element can't be moved into itself" } else { "an element can't be moved before or after itself" }));
    }
    let e = tree.find(element).ok_or(TagError::NotAnElement(element.num))?;
    let t = tree.find(target).ok_or(TagError::NotAnElement(target.num))?;
    if tree.ancestors(t).any(|a| a == e) {
        return Err(invalid(format!("element {} is inside element {}: an element can't be moved into itself", target.num, element.num)));
    }
    let from = parent_obj(&tree, e)
        .ok_or_else(|| invalid(format!("element {}'s parent is written inside its own parent and can't be edited", element.num)))?;
    let to = if into {
        Some(target)
    } else {
        parent_obj(&tree, t)
            .ok_or_else(|| invalid(format!("element {}'s parent is written inside its own parent and can't be edited", target.num)))?
    };
    let mut work = doc.clone();
    let holder = from.unwrap_or(root);
    let d = work.get(holder).as_dict().cloned().ok_or_else(|| invalid("a structure element is not a dictionary"))?;
    let kids = kid_list(&work, &d);
    let keep: Vec<Object> = kids.iter().filter(|k| k.as_ref() != Some(element)).cloned().collect();
    if keep.len() == kids.len() {
        return Err(invalid(format!("element {} is not listed in its parent's kids; it can't be moved", element.num)));
    }
    work.update_dict(holder, |d| d.set(b"K".to_vec(), Object::Array(keep)))?;
    let holder_to = to.unwrap_or(root);
    let d = work.get(holder_to).as_dict().cloned().ok_or_else(|| invalid("a structure element is not a dictionary"))?;
    if !into && !kid_list(&work, &d).iter().any(|k| k.as_ref() == Some(target)) {
        return Err(invalid(format!("element {} is not listed in its parent's kids; nothing can be moved next to it", target.num)));
    }
    insert_kid(&mut work, root, to, if into { None } else { Some(target) }, after, element)?;
    work.update_dict(element, |d| d.set(b"P".to_vec(), Object::Ref(holder_to)))?;
    *doc = work;
    Ok(())
}

/// What [`clear_page_structure`] removed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClearResult {
    /// Tagged (MCID) marked-content sequences taken out of the content.
    pub mcids: usize,
    /// Artifact sequences taken out of the content.
    pub artifacts: usize,
    /// Marked-content and object references taken out of elements.
    pub references: usize,
    /// Elements that left the tree because they were left empty.
    pub removed: usize,
    /// Annotations that lost their /StructParent.
    pub annotations: usize,
}

/// Reading Order tool ▸ Clear Page Structure: the page's tagged marked content (and, unless
/// `keep_artifacts`, its artifacts) leave the content stream, its MCIDs and annotations leave
/// the elements (elements left empty leave the tree), and its parent-tree entries go. The page's
/// content is then untagged and can be tagged again region by region. All or nothing.
pub fn clear_page_structure(doc: &mut Document, page: usize, keep_artifacts: bool) -> Result<ClearResult, TagError> {
    let pages = pdfcraft_model::pages(doc);
    let pg =
        pages.get(page).ok_or_else(|| invalid(format!("there is no page {}: the document has {} pages", page.saturating_add(1), pages.len())))?;
    let number = page.saturating_add(1);
    let tree = tags(doc);
    if tree.exists && tree.truncated {
        return Err(invalid("the structure tree is too large or too deeply nested to edit safely"));
    }
    let mut result = ClearResult::default();
    let parsed = pdfcraft_content::parse(&page_content(doc, &pg.dict));
    let props = sub_dict(doc, &sub_dict(doc, &pg.dict, b"Resources"), b"Properties");
    let mut stack: Vec<bool> = Vec::new();
    let mut out: Vec<Op> = Vec::with_capacity(parsed.ops.len());
    for op in &parsed.ops {
        match op.op.as_slice() {
            b"BMC" | b"BDC" => {
                if stack.len() >= MAX_NESTING {
                    return Err(invalid(format!("page {number} nests marked content too deeply to change")));
                }
                let mcid = mc_props(doc, op, &props).is_some_and(|d| d.contains(b"MCID"));
                let artifact = op.name(0) == Some(b"Artifact");
                let drop = mcid || (artifact && !keep_artifacts);
                if drop {
                    if mcid {
                        result.mcids = result.mcids.saturating_add(1);
                    } else {
                        result.artifacts = result.artifacts.saturating_add(1);
                    }
                } else {
                    out.push(op.clone());
                }
                stack.push(drop);
            }
            b"EMC" => match stack.pop() {
                Some(true) => {}
                _ => out.push(op.clone()),
            },
            _ => out.push(op.clone()),
        }
    }
    let annots: Vec<ObjRef> = pg
        .dict
        .get(b"Annots")
        .map(|a| doc.resolve(a))
        .and_then(|a| a.as_array().map(|a| a.iter().filter_map(Object::as_ref).take(100_000).collect()))
        .unwrap_or_default();
    let annot_set: HashSet<ObjRef> = annots.iter().copied().collect();
    let mut keys: HashSet<i64> = HashSet::new();
    if let Some(k) = pg.dict.get(b"StructParents").and_then(Object::as_int) {
        keys.insert(k);
    }
    let tagged_annots: Vec<ObjRef> = annots.iter().copied().filter(|a| doc.get(*a).as_dict().is_some_and(|d| d.contains(b"StructParent"))).collect();
    for a in &tagged_annots {
        if let Some(k) = doc.get(*a).as_dict().and_then(|d| d.get(b"StructParent").and_then(Object::as_int)) {
            keys.insert(k);
        }
    }
    // The elements holding the page's content.
    let holders: Vec<usize> = tree
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| {
            n.kids.iter().any(|k| match k {
                TagKid::Content { page: Some(p), stream: None, .. } => *p == page,
                TagKid::Object { obj, .. } => annot_set.contains(obj),
                _ => false,
            })
        })
        .map(|(i, _)| i)
        .collect();
    if result.mcids == 0 && result.artifacts == 0 && holders.is_empty() && keys.is_empty() {
        return Err(invalid(format!("page {number} has no tags{} to clear", if keep_artifacts { "" } else { " or artifacts" })));
    }
    if parsed.skipped > 0 && (result.mcids > 0 || result.artifacts > 0) {
        return Err(invalid(format!("page {number}'s content has bytes that can't be read; it is left unchanged so nothing is lost")));
    }
    if let Some(i) = holders.iter().find(|i| tree.nodes.get(**i).is_some_and(|n| n.obj.is_none())) {
        let ty = tree.nodes.get(*i).map(|n| n.ty.clone()).unwrap_or_default();
        return Err(invalid(format!("a {ty} element written inside its parent holds content of page {number}; it can't be edited")));
    }
    let mut work = doc.clone();
    if result.mcids > 0 || result.artifacts > 0 {
        let st = work.add(Object::Stream(Stream::flate(Dict::new(), &pdfcraft_content::serialize_ops(&out))));
        work.update_dict(pg.obj, |d| d.set(b"Contents".to_vec(), Object::Ref(st)))?;
    }
    if tree.exists {
        let root = root_ref(&work)?;
        let all: HashSet<i64> = tree
            .nodes
            .iter()
            .flat_map(|n| n.kids.iter())
            .filter_map(|k| match k {
                TagKid::Content { page: Some(p), mcid, stream: None } if *p == page => Some(*mcid),
                _ => None,
            })
            .collect();
        let mut emptied = Vec::new();
        for i in &holders {
            let Some(n) = tree.nodes.get(*i) else { continue };
            let Some(o) = n.obj else { continue };
            let (removed, empty) = strip(&mut work, o, n.page == Some(page), pg.obj, &all, &annot_set)?;
            result.references = result.references.saturating_add(removed);
            if empty {
                emptied.push(o);
            }
        }
        let parents: HashMap<ObjRef, Option<ObjRef>> =
            tree.nodes.iter().enumerate().filter_map(|(i, n)| n.obj.and_then(|o| parent_obj(&tree, i).map(|p| (o, p)))).collect();
        let parent_of = |e: ObjRef| parents.get(&e).copied();
        result.removed = prune(&mut work, root, emptied, &parent_of)?;
        // Its parent-tree entries go.
        if !keys.is_empty()
            && let Some(Object::Ref(pt)) = work.get(root).as_dict().and_then(|d| d.get(b"ParentTree").cloned())
        {
            for node in number_tree_nodes(&work, pt) {
                let Some(list) = nums(&work, node) else { continue };
                let mut keep = Vec::with_capacity(list.len());
                let mut cut = false;
                for pair in list.chunks(2) {
                    if pair.first().and_then(Object::as_int).is_some_and(|k| keys.contains(&k)) {
                        cut = true;
                    } else {
                        keep.extend(pair.iter().cloned());
                    }
                }
                if cut {
                    work.update_dict(node, |d| d.set(b"Nums".to_vec(), Object::Array(keep)))?;
                }
            }
        }
    }
    if pg.dict.contains(b"StructParents") {
        work.update_dict(pg.obj, |d| {
            d.remove(b"StructParents");
        })?;
    }
    for a in &tagged_annots {
        work.update_dict(*a, |d| {
            d.remove(b"StructParent");
        })?;
        result.annotations = result.annotations.saturating_add(1);
    }
    *doc = work;
    Ok(result)
}
