//! Change to Artifact (Reading Order tool, Content and Tags panels): page content that is
//! decoration rather than document content (running headers and footers, page numbers, rules,
//! backgrounds, watermarks) is wrapped in `/Artifact` marked content, and the structure
//! elements that pointed at it lose those marked-content references (elements left empty
//! leave the tree), so assistive technology and accessible-text export skip it.
//!
//! Content is chosen on a page by a rectangle, by marked-content ids, or by structure elements
//! (everything they cover, on any page). Inside the rectangle, tagged marked content changes
//! to an artifact as a whole when everything it paints lies inside; untagged painting
//! operations (text runs, paths, images) are wrapped one by one, so an untagged document can
//! have artifacts too. Text is located by where each run starts (its baseline origin), paths by
//! their points and images and forms by their boxes.
//!
//! The rewrite keeps the page's `q`/`Q`, `BT`/`ET` and marked-content nesting: a tagged
//! sequence changes only its opening operator (`/P <</MCID 3>> BDC` becomes
//! `/Artifact <<…>> BDC`), and new artifact sequences open and close between whole operations,
//! never across `q`, `Q`, `BT`, `ET` or another marked-content operator, and never inside a path.
//! Every walk is bounded; nothing changes unless all of it can.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use pdfcraft_content::{Matrix, Op};
use pdfcraft_cos::{Dict, Document, ObjRef, Object, Stream};

use crate::tags::{MAX_MCID, TagError, TagKid, catalog, number_tree_leaf, page_content, sub_dict, tags};

/// The most marked-content ids or elements one call may name.
pub const MAX_ARTIFACT_SELECTION: usize = 100_000;
/// Marked content nested deeper than this is refused.
const MAX_NESTING: usize = 4096;
/// Slack when testing that content lies inside the rectangle (points).
const EPS: f64 = 0.5;

/// /Type of an artifact (ISO 32000-2 §14.8.2.2, Table 363).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtifactType {
    Pagination,
    Layout,
    Page,
    Background,
}

impl ArtifactType {
    pub const ALL: [ArtifactType; 4] = [ArtifactType::Pagination, ArtifactType::Layout, ArtifactType::Page, ArtifactType::Background];

    pub fn name(self) -> &'static str {
        match self {
            ArtifactType::Pagination => "Pagination",
            ArtifactType::Layout => "Layout",
            ArtifactType::Page => "Page",
            ArtifactType::Background => "Background",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.name().eq_ignore_ascii_case(s))
    }
}

/// /Subtype of a pagination artifact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtifactSubtype {
    Header,
    Footer,
    Watermark,
}

impl ArtifactSubtype {
    pub const ALL: [ArtifactSubtype; 3] = [ArtifactSubtype::Header, ArtifactSubtype::Footer, ArtifactSubtype::Watermark];

    pub fn name(self) -> &'static str {
        match self {
            ArtifactSubtype::Header => "Header",
            ArtifactSubtype::Footer => "Footer",
            ArtifactSubtype::Watermark => "Watermark",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.name().eq_ignore_ascii_case(s))
    }
}

/// What to change to an artifact.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArtifactSelection {
    /// 0-based page of `rect` and `mcids`.
    pub page: Option<usize>,
    /// A rectangle in the page's user space `[x0, y0, x1, y1]`.
    pub rect: Option<[f64; 4]>,
    /// Marked-content ids on `page`.
    pub mcids: Vec<i64>,
    /// Structure elements, by object number: they and everything they cover.
    pub elements: Vec<u32>,
    /// The artifact's /Type (a subtype alone implies Pagination).
    pub ty: Option<ArtifactType>,
    /// The artifact's /Subtype (pagination artifacts only).
    pub subtype: Option<ArtifactSubtype>,
}

/// What [`mark_artifact`] changed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArtifactResult {
    /// Tagged marked content changed to artifacts: (0-based page, MCID), sorted.
    pub marked: Vec<(usize, i64)>,
    /// Untagged painting operations wrapped as artifacts.
    pub untagged: usize,
    /// Structure elements that left the tree (those named, those inside them, and those left
    /// empty).
    pub elements_removed: usize,
    /// Pages whose content changed (0-based).
    pub pages: Vec<usize>,
}

fn invalid(s: impl Into<String>) -> TagError {
    TagError::Invalid(s.into())
}

/// Does the operator paint or build a path (content an artifact can hold)?
fn is_content(op: &[u8]) -> bool {
    matches!(
        op,
        b"Tj"
            | b"TJ"
            | b"'"
            | b"\""
            | b"Do"
            | b"BI"
            | b"sh"
            | b"m"
            | b"l"
            | b"c"
            | b"v"
            | b"y"
            | b"h"
            | b"re"
            | b"W"
            | b"W*"
            | b"f"
            | b"F"
            | b"f*"
            | b"B"
            | b"B*"
            | b"b"
            | b"b*"
            | b"S"
            | b"s"
            | b"n"
    )
}

/// Operators a new artifact sequence never spans.
fn is_structural(op: &[u8]) -> bool {
    matches!(op, b"q" | b"Q" | b"BT" | b"ET" | b"BMC" | b"BDC" | b"EMC" | b"BX" | b"EX" | b"MP" | b"DP")
}

fn is_path_build(op: &[u8]) -> bool {
    matches!(op, b"m" | b"l" | b"c" | b"v" | b"y" | b"h" | b"re" | b"W" | b"W*")
}

fn is_path_paint(op: &[u8]) -> bool {
    matches!(op, b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" | b"S" | b"s")
}

fn union(a: Option<[f64; 4]>, b: [f64; 4]) -> [f64; 4] {
    match a {
        Some(a) => [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])],
        None => b,
    }
}

/// One marked-content sequence of the page.
struct Seq {
    mcid: Option<i64>,
    artifact: bool,
    parent: Option<usize>,
    /// It paints something, and everything it paints is inside the rectangle.
    paints: bool,
    inside: bool,
}

/// Untagged painting ops `start..=end` (one text run, one path, one image).
struct Group {
    start: usize,
    end: usize,
    inside: bool,
}

/// A page's content read for the change.
struct Scan {
    ops: Vec<Op>,
    skipped: usize,
    seqs: Vec<Seq>,
    groups: Vec<Group>,
    /// The sequence each BMC/BDC op opens.
    opens: HashMap<usize, usize>,
}

fn scan(doc: &Document, page: &Dict, rect: Option<[f64; 4]>, number: usize) -> Result<Scan, TagError> {
    let parsed = pdfcraft_content::parse(&page_content(doc, page));
    let res = sub_dict(doc, page, b"Resources");
    let xobjects = sub_dict(doc, &res, b"XObject");
    let props = sub_dict(doc, &res, b"Properties");
    let inside = |b: [f64; 4]| rect.is_some_and(|r| pdfcraft_content::contains(r, b, EPS));
    let mut ctm = vec![Matrix::IDENTITY];
    let (mut tm, mut tlm, mut leading) = (Matrix::IDENTITY, Matrix::IDENTITY, 0.0f64);
    let mut stack: Vec<usize> = Vec::new();
    let mut seqs: Vec<Seq> = Vec::new();
    let mut groups: Vec<Group> = Vec::new();
    let mut opens = HashMap::new();
    // The path being built: its first op, its box, and whether something else interrupted it.
    let mut path: Option<(usize, Option<[f64; 4]>, bool)> = None;
    for (i, op) in parsed.ops.iter().enumerate() {
        let top = ctm.last().copied().unwrap_or(Matrix::IDENTITY);
        let name = op.op.as_slice();
        if let Some(p) = path.as_mut()
            && !is_path_build(name)
            && !is_path_paint(name)
            && name != b"n"
        {
            p.2 = true;
        }
        // What this op paints: its box.
        let mut painted: Option<[f64; 4]> = None;
        let mut group_start = i;
        match name {
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
            b"BMC" | b"BDC" => {
                if stack.len() >= MAX_NESTING {
                    return Err(invalid(format!("page {number} nests marked content too deeply to change")));
                }
                let p = match op.operands.get(1) {
                    Some(Object::Dict(d)) => Some(d.clone()),
                    Some(Object::Name(n)) if name == b"BDC" => props.get(n).map(|x| doc.resolve(x)).and_then(|x| x.as_dict().cloned()),
                    _ => None,
                };
                let mcid = if name == b"BDC" { p.as_ref().and_then(|d| d.get(b"MCID").and_then(Object::as_int)) } else { None };
                opens.insert(i, seqs.len());
                stack.push(seqs.len());
                seqs.push(Seq {
                    mcid,
                    artifact: op.name(0) == Some(b"Artifact"),
                    parent: stack.iter().rev().nth(1).copied(),
                    paints: false,
                    inside: true,
                });
            }
            b"EMC" => {
                stack.pop();
            }
            b"BT" => {
                tm = Matrix::IDENTITY;
                tlm = Matrix::IDENTITY;
            }
            b"TL" => leading = op.num(0).unwrap_or(leading),
            b"Td" | b"TD" => {
                if let Some([x, y]) = op.nums::<2>() {
                    if name == b"TD" {
                        leading = -y;
                    }
                    tlm = Matrix::translate(x, y).then(&tlm);
                    tm = tlm;
                }
            }
            b"Tm" => {
                if let Some(m) = Matrix::from_operands(&op.operands) {
                    tlm = m;
                    tm = m;
                }
            }
            b"T*" => {
                tlm = Matrix::translate(0.0, -leading).then(&tlm);
                tm = tlm;
            }
            b"Tj" | b"TJ" | b"'" | b"\"" => {
                if matches!(name, b"'" | b"\"") {
                    tlm = Matrix::translate(0.0, -leading).then(&tlm);
                    tm = tlm;
                }
                let (x, y) = tm.then(&top).apply(0.0, 0.0);
                painted = Some([x, y, x, y]);
            }
            b"Do" => {
                let target = op.name(0).and_then(|n| xobjects.get(n)).map(|x| doc.resolve(x));
                let b = match target.as_deref() {
                    Some(Object::Stream(s)) if s.dict.name(b"Subtype") == Some(b"Form") => {
                        let bb: Vec<f64> =
                            s.dict.get(b"BBox").and_then(|b| b.as_array().map(|a| a.iter().filter_map(Object::as_f64).collect())).unwrap_or_default();
                        let m = Matrix::from_operands(s.dict.get(b"Matrix").and_then(|m| m.as_array()).map(Vec::as_slice).unwrap_or(&[]))
                            .unwrap_or(Matrix::IDENTITY);
                        match bb[..] {
                            [a, b, c, d] => m.then(&top).bbox([a, b, c, d]),
                            _ => top.bbox([0.0, 0.0, 1.0, 1.0]),
                        }
                    }
                    _ => top.bbox([0.0, 0.0, 1.0, 1.0]),
                };
                painted = Some(b);
            }
            b"BI" => painted = Some(top.bbox([0.0, 0.0, 1.0, 1.0])),
            b"m" | b"l" | b"c" | b"v" | b"y" | b"re" => {
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
                    _ => {
                        if let Some([x, y]) = op.nums::<2>() {
                            add(x, y);
                        }
                    }
                }
            }
            b"h" | b"W" | b"W*" => {
                path.get_or_insert((i, None, false));
            }
            b"n" => {
                path = None;
            }
            _ if is_path_paint(name) => {
                if let Some((start, b, tainted)) = path.take() {
                    if !tainted && let Some(b) = b {
                        painted = Some(b);
                        group_start = start;
                    } else if let Some(b) = b {
                        // Painted, but not wrappable on its own: it still counts for the
                        // marked content around it.
                        mark_painted(&mut seqs, &stack, inside(b));
                    }
                }
            }
            _ => {}
        }
        let Some(b) = painted else { continue };
        let finite = b.iter().all(|v| v.is_finite());
        let within = finite && inside(b);
        mark_painted(&mut seqs, &stack, within);
        let tagged = stack.iter().any(|s| seqs.get(*s).is_some_and(|q| q.mcid.is_some() || q.artifact));
        if !tagged && finite {
            groups.push(Group { start: group_start, end: i, inside: within });
        }
    }
    Ok(Scan { ops: parsed.ops, skipped: parsed.skipped, seqs, groups, opens })
}

fn mark_painted(seqs: &mut [Seq], stack: &[usize], inside: bool) {
    for s in stack {
        if let Some(q) = seqs.get_mut(*s) {
            q.paints = true;
            q.inside &= inside;
        }
    }
}

/// The rewritten content of one page.
struct PagePlan {
    obj: ObjRef,
    ops: Vec<Op>,
    marked: BTreeSet<i64>,
    untagged: usize,
}

fn artifact_props(ty: Option<ArtifactType>, subtype: Option<ArtifactSubtype>) -> Option<Dict> {
    let ty = ty?;
    let mut d = Dict::new();
    d.set(b"Type".to_vec(), Object::name(ty.name()));
    if let Some(s) = subtype {
        d.set(b"Subtype".to_vec(), Object::name(s.name()));
    }
    Some(d)
}

fn opening(props: &Option<Dict>) -> Op {
    match props {
        Some(d) => Op::new("BDC", vec![Object::name("Artifact"), Object::Dict(d.clone())]),
        None => Op::new("BMC", vec![Object::name("Artifact")]),
    }
}

/// Plan one page: the MCIDs in `want`, and with `rect` the tagged sequences and untagged
/// operations inside it. `None` when nothing on the page changes.
fn plan_page(
    doc: &Document,
    page: &pdfcraft_model::Page,
    number: usize,
    want: &BTreeSet<i64>,
    rect: Option<[f64; 4]>,
    props: &Option<Dict>,
    found: &mut BTreeSet<i64>,
) -> Result<Option<PagePlan>, TagError> {
    let s = scan(doc, &page.dict, rect, number)?;
    found.extend(s.seqs.iter().filter_map(|q| q.mcid));
    // Which sequences change: named, inside the rectangle, or inside one that changes.
    let mut convert = vec![false; s.seqs.len()];
    // Inside a sequence that changes (through any marked content, such as optional content,
    // which keeps its own operator).
    let mut under = vec![false; s.seqs.len()];
    for (k, q) in s.seqs.iter().enumerate() {
        let by_parent = q.parent.and_then(|p| under.get(p).copied()).unwrap_or(false);
        let own = q.mcid.is_some_and(|m| want.contains(&m) || (rect.is_some() && q.paints && q.inside && !q.artifact));
        if let Some(u) = under.get_mut(k) {
            *u = own || by_parent;
        }
        if let Some(c) = convert.get_mut(k) {
            *c = q.mcid.is_some() && (own || by_parent);
        }
    }
    let mut wrap = vec![false; s.ops.len()];
    let mut untagged = 0usize;
    if rect.is_some() {
        for g in s.groups.iter().filter(|g| g.inside) {
            untagged += 1;
            for w in wrap.iter_mut().take(g.end.saturating_add(1)).skip(g.start) {
                *w = true;
            }
        }
    }
    let marked: BTreeSet<i64> = s.seqs.iter().zip(&convert).filter(|(_, c)| **c).filter_map(|(q, _)| q.mcid).collect();
    if marked.is_empty() && untagged == 0 {
        return Ok(None);
    }
    if s.skipped > 0 {
        return Err(invalid(format!("page {number}'s content has bytes that can't be read; it is left unchanged so nothing is lost")));
    }
    let mut out = Vec::with_capacity(s.ops.len().saturating_add(untagged.saturating_mul(2)));
    let mut open = false;
    for (i, op) in s.ops.into_iter().enumerate() {
        let selected = wrap.get(i).copied().unwrap_or(false);
        let name = op.op.as_slice();
        if open && !selected && (is_structural(name) || is_content(name)) {
            out.push(Op::new("EMC", Vec::new()));
            open = false;
        }
        if selected && !open {
            out.push(opening(props));
            open = true;
        }
        let converts = s.opens.get(&i).and_then(|k| convert.get(*k)).copied().unwrap_or(false);
        if converts {
            // The sequence keeps its EMC: BMC and BDC both take one.
            out.push(opening(props));
        } else {
            out.push(op);
        }
    }
    if open {
        out.push(Op::new("EMC", Vec::new()));
    }
    Ok(Some(PagePlan { obj: page.obj, ops: out, marked, untagged }))
}

/// Change the selected content to an artifact, all or nothing (see the module docs).
pub fn mark_artifact(doc: &mut Document, sel: &ArtifactSelection) -> Result<ArtifactResult, TagError> {
    if sel.rect.is_none() && sel.mcids.is_empty() && sel.elements.is_empty() {
        return Err(invalid("select the content: give a rectangle on a page, marked-content ids (mcids) on a page, or structure elements"));
    }
    if sel.mcids.len().saturating_add(sel.elements.len()) > MAX_ARTIFACT_SELECTION {
        return Err(invalid(format!("select at most {MAX_ARTIFACT_SELECTION} marked-content ids and elements at once")));
    }
    let ty = match (sel.ty, sel.subtype) {
        (Some(t), Some(_)) if t != ArtifactType::Pagination => {
            return Err(invalid("only pagination artifacts have a subtype (Header, Footer or Watermark); use type Pagination"));
        }
        (None, Some(_)) => Some(ArtifactType::Pagination),
        (t, _) => t,
    };
    let props = artifact_props(ty, sel.subtype);
    let pages = pdfcraft_model::pages(doc);
    let page = match (sel.page, sel.rect.is_some() || !sel.mcids.is_empty()) {
        (Some(p), _) if p >= pages.len() => {
            return Err(invalid(format!("there is no page {}: the document has {} pages", p.saturating_add(1), pages.len())));
        }
        (None, true) => return Err(invalid("give the page the rectangle or marked-content ids are on")),
        (p, _) => p,
    };
    let rect = match sel.rect {
        None => None,
        Some(r) => {
            if !r.iter().all(|v| v.is_finite()) {
                return Err(invalid("rect must be four finite numbers"));
            }
            let r = [r[0].min(r[2]), r[1].min(r[3]), r[0].max(r[2]), r[1].max(r[3])];
            if r[2] - r[0] <= 0.0 || r[3] - r[1] <= 0.0 {
                return Err(invalid("rect must have a width and a height"));
            }
            Some(r)
        }
    };
    if let Some(m) = sel.mcids.iter().find(|m| !(0..MAX_MCID).contains(*m)) {
        return Err(invalid(format!("{m} is not a marked-content id (they run from 0)")));
    }

    let tree = tags(doc);
    let needs_tree = !sel.mcids.is_empty() || !sel.elements.is_empty();
    if needs_tree && !tree.exists {
        return Err(invalid(
            "the document is not tagged, so it has no marked-content ids or elements to select; select the content with a rectangle instead",
        ));
    }
    if tree.exists && tree.truncated {
        return Err(invalid("the structure tree is too large or too deeply nested to edit safely"));
    }

    // Elements named, with everything inside them (pre-order: the nodes after one that are
    // deeper are its descendants).
    let mut removed: BTreeSet<usize> = BTreeSet::new();
    let mut named: BTreeSet<usize> = BTreeSet::new();
    for e in &sel.elements {
        let i = tree.nodes.iter().position(|n| n.obj.is_some_and(|o| o.num == *e)).ok_or(TagError::NotAnElement(*e))?;
        named.insert(i);
        let depth = tree.nodes.get(i).map_or(0, |n| n.depth);
        removed.insert(i);
        for (j, n) in tree.nodes.iter().enumerate().skip(i.saturating_add(1)) {
            if n.depth <= depth {
                break;
            }
            removed.insert(j);
        }
    }
    // Their content, page by page.
    let mut want: BTreeMap<usize, BTreeSet<i64>> = BTreeMap::new();
    for &i in &removed {
        let Some(n) = tree.nodes.get(i) else { continue };
        let num = n.obj.map_or(0, |o| o.num);
        for k in &n.kids {
            match k {
                TagKid::Content { page: Some(p), mcid, stream: None } => {
                    want.entry(*p).or_default().insert(*mcid);
                }
                TagKid::Content { stream: Some(_), .. } => {
                    return Err(invalid(format!("element {num} marks content inside a form XObject, which can't be changed to an artifact yet")));
                }
                TagKid::Content { page: None, .. } => {
                    return Err(invalid(format!("element {num} marks content without saying which page it is on")));
                }
                TagKid::Object { .. } => {
                    return Err(invalid(format!("element {num} holds an annotation or object reference; only page content can become an artifact")));
                }
                TagKid::Element(_) => {}
            }
        }
    }
    let from_elements: BTreeMap<usize, BTreeSet<i64>> = want.clone();
    if let Some(p) = page {
        want.entry(p).or_default().extend(sel.mcids.iter().copied());
    }

    // The page rewrites.
    let mut plans: Vec<(usize, PagePlan)> = Vec::new();
    let mut targets: BTreeSet<usize> = want.keys().copied().collect();
    if let Some(p) = page {
        targets.insert(p);
    }
    for p in targets {
        let Some(pg) = pages.get(p) else { continue };
        let empty = BTreeSet::new();
        let w = want.get(&p).unwrap_or(&empty);
        let mut found = BTreeSet::new();
        let r = if Some(p) == page { rect } else { None };
        let plan = plan_page(doc, pg, p.saturating_add(1), w, r, &props, &mut found)?;
        if Some(p) == page
            && let Some(m) = sel.mcids.iter().find(|m| !found.contains(*m))
        {
            return Err(invalid(format!("page {} has no marked content with MCID {m}", p.saturating_add(1))));
        }
        if let Some(plan) = plan {
            plans.push((p, plan));
        }
    }
    // The content that leaves the tree: what changed, and what the named elements covered.
    let mut gone: HashSet<(usize, i64)> = HashSet::new();
    for (p, plan) in &plans {
        gone.extend(plan.marked.iter().map(|m| (*p, *m)));
    }
    for (p, ms) in &from_elements {
        gone.extend(ms.iter().map(|m| (*p, *m)));
    }
    if plans.is_empty() && removed.is_empty() {
        return Err(match (page, rect) {
            (Some(p), Some(_)) => invalid(format!("nothing on page {} lies inside the rectangle to mark as an artifact", p.saturating_add(1))),
            _ => invalid("nothing selected to mark as an artifact"),
        });
    }

    // The tree edits: elements that lose kids, and elements left empty (deepest first).
    let mut changed: BTreeSet<usize> = BTreeSet::new();
    let mut remaining: Vec<usize> = tree.nodes.iter().map(|n| n.kids.len()).collect();
    for (i, n) in tree.nodes.iter().enumerate() {
        if removed.contains(&i) {
            continue;
        }
        let lost =
            n.kids.iter().filter(|k| matches!(k, TagKid::Content { page: Some(p), mcid, stream: None } if gone.contains(&(*p, *mcid)))).count();
        if lost > 0 {
            changed.insert(i);
            if let Some(r) = remaining.get_mut(i) {
                *r = r.saturating_sub(lost);
            }
        }
    }
    let mut pruned: BTreeSet<usize> = BTreeSet::new();
    for i in (0..tree.nodes.len()).rev() {
        let Some(n) = tree.nodes.get(i) else { continue };
        let leaves = removed.contains(&i) || pruned.contains(&i);
        if !leaves && changed.contains(&i) && remaining.get(i) == Some(&0) && n.depth > 0 {
            pruned.insert(i);
        }
        if ((removed.contains(&i) && named.contains(&i)) || pruned.contains(&i))
            && let Some(p) = n.parent
            && !removed.contains(&p)
        {
            changed.insert(p);
            if let Some(r) = remaining.get_mut(p) {
                *r = r.saturating_sub(1);
            }
        }
    }
    let leaving: HashSet<ObjRef> = removed.iter().chain(&pruned).filter_map(|i| tree.nodes.get(*i).and_then(|n| n.obj)).collect();
    // Each change is written in the nearest element that is an object of its own (an element
    // written inside its parent is rewritten with it), else in the tree root's kids.
    let mut rewrite: BTreeMap<ObjRef, Option<usize>> = BTreeMap::new();
    let mut root_rewrite = named.iter().any(|i| tree.nodes.get(*i).is_some_and(|n| n.parent.is_none()));
    for &i in &changed {
        if removed.contains(&i) || pruned.contains(&i) {
            continue;
        }
        let mut at = Some(i);
        let mut steps = 0usize;
        while let Some(j) = at {
            let Some(n) = tree.nodes.get(j) else { break };
            if let Some(obj) = n.obj
                && !pruned.contains(&j)
            {
                rewrite.insert(obj, n.page);
                break;
            }
            at = n.parent;
            steps += 1;
            if at.is_none() {
                root_rewrite = true;
            }
            if steps > tree.nodes.len() {
                break;
            }
        }
    }
    let root_ref = catalog(doc).and_then(|(_, c)| c.get(b"StructTreeRoot").and_then(Object::as_ref));
    if root_rewrite && root_ref.is_none() {
        return Err(invalid("the structure tree root is not an object of its own; it can't be edited"));
    }

    // Everything checked: change it.
    let mut result = ArtifactResult::default();
    for (p, plan) in &plans {
        let s = doc.add(Object::Stream(Stream::flate(Dict::new(), &pdfcraft_content::serialize_ops(&plan.ops))));
        doc.update_dict(plan.obj, |d| d.set(b"Contents".to_vec(), Object::Ref(s)))?;
        result.marked.extend(plan.marked.iter().map(|m| (*p, *m)));
        result.untagged = result.untagged.saturating_add(plan.untagged);
        result.pages.push(*p);
    }
    let page_index: HashMap<ObjRef, usize> = pages.iter().enumerate().map(|(i, p)| (p.obj, i)).collect();
    for (obj, page) in rewrite {
        let k = doc.get(obj).as_dict().and_then(|d| d.get(b"K").cloned());
        if let Some(k) = k {
            let new = filter_kids(doc, &k, page, &page_index, &gone, &leaving, 0);
            doc.update_dict(obj, |d| d.set(b"K".to_vec(), new))?;
        }
    }
    if root_rewrite && let Some(r) = root_ref {
        let k = doc.get(r).as_dict().and_then(|d| d.get(b"K").cloned());
        if let Some(k) = k {
            let new = filter_kids(doc, &k, None, &page_index, &gone, &leaving, 0);
            doc.update_dict(r, |d| d.set(b"K".to_vec(), new))?;
        }
    }
    // The parent tree no longer maps those MCIDs to elements.
    let mut by_page: BTreeMap<usize, BTreeSet<i64>> = BTreeMap::new();
    for (p, m) in &gone {
        by_page.entry(*p).or_default().insert(*m);
    }
    for (p, ms) in by_page {
        if let Some(pg) = pages.get(p) {
            clear_parent_tree(doc, &pg.dict, &ms)?;
        }
    }
    result.elements_removed = removed.len().saturating_add(pruned.len());
    Ok(result)
}

/// Elements written inside their parents are filtered this deep, no deeper.
const MAX_DIRECT_DEPTH: usize = 64;

/// An element's /K without the marked content in `gone` and the elements in `leaving`.
/// Elements written inside it are filtered too, and dropped when that leaves them empty.
fn filter_kids(
    doc: &Document,
    k: &Object,
    page: Option<usize>,
    page_index: &HashMap<ObjRef, usize>,
    gone: &HashSet<(usize, i64)>,
    leaving: &HashSet<ObjRef>,
    depth: usize,
) -> Object {
    let mcr_gone = |d: &Dict| {
        if d.name(b"Type") != Some(b"MCR") || d.contains(b"Stm") {
            return false;
        }
        let p = d.get(b"Pg").and_then(Object::as_ref).and_then(|r| page_index.get(&r).copied()).or(page);
        match (p, d.get(b"MCID").and_then(Object::as_int)) {
            (Some(p), Some(m)) => gone.contains(&(p, m)),
            _ => false,
        }
    };
    let keep = |o: &Object| match o {
        Object::Int(m) => !page.is_some_and(|p| gone.contains(&(p, *m))),
        Object::Dict(d) => !mcr_gone(d),
        Object::Ref(r) => !leaving.contains(r) && !doc.get(*r).as_dict().is_some_and(mcr_gone),
        _ => true,
    };
    let list = match k {
        Object::Array(a) => a.clone(),
        Object::Ref(r) if !leaving.contains(r) => match doc.get(*r).as_array() {
            Some(a) => a.clone(),
            None => vec![k.clone()],
        },
        other => vec![other.clone()],
    };
    let mut out = Vec::with_capacity(list.len());
    for o in list.into_iter().filter(keep) {
        match o {
            Object::Dict(mut d) if d.contains(b"S") && depth < MAX_DIRECT_DEPTH => {
                let inner_page = d.get(b"Pg").and_then(Object::as_ref).and_then(|r| page_index.get(&r).copied()).or(page);
                if let Some(inner) = d.get(b"K").cloned() {
                    let before = match &inner {
                        Object::Array(a) => a.len(),
                        _ => 1,
                    };
                    let after = filter_kids(doc, &inner, inner_page, page_index, gone, leaving, depth.saturating_add(1));
                    if matches!(&after, Object::Array(a) if a.is_empty() && before > 0) {
                        continue;
                    }
                    d.set(b"K".to_vec(), after);
                }
                out.push(Object::Dict(d));
            }
            other => out.push(other),
        }
    }
    Object::Array(out)
}

/// Null the page's parent-tree entries for `mcids`.
fn clear_parent_tree(doc: &mut Document, page: &Dict, mcids: &BTreeSet<i64>) -> Result<(), TagError> {
    let Some(key) = page.get(b"StructParents").and_then(Object::as_int) else { return Ok(()) };
    let root = catalog(doc).and_then(|(_, c)| c.get(b"StructTreeRoot").map(|o| doc.resolve(o)).and_then(|o| o.as_dict().cloned()));
    let Some(pt) = root.and_then(|r| r.get(b"ParentTree").and_then(Object::as_ref)) else { return Ok(()) };
    let Some((node, mut nums)) = number_tree_leaf(doc, pt, key) else { return Ok(()) };
    let clear = |a: &mut Vec<Object>| {
        for m in mcids {
            if let Some(slot) = usize::try_from(*m).ok().and_then(|i| a.get_mut(i)) {
                *slot = Object::Null;
            }
        }
    };
    let mut nums_changed = false;
    for pair in nums.chunks_mut(2) {
        if pair.first().and_then(Object::as_int) != Some(key) {
            continue;
        }
        match pair.get(1).cloned() {
            Some(Object::Ref(r)) => {
                if let Some(mut a) = doc.get(r).as_array().cloned() {
                    clear(&mut a);
                    doc.set(r, Object::Array(a));
                }
            }
            Some(Object::Array(mut a)) => {
                clear(&mut a);
                if let Some(v) = pair.get_mut(1) {
                    *v = Object::Array(a);
                    nums_changed = true;
                }
            }
            _ => {}
        }
    }
    if nums_changed {
        doc.update_dict(node, |d| d.set(b"Nums".to_vec(), Object::Array(nums)))?;
    }
    Ok(())
}
