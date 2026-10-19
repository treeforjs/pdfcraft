//! Autotag Document (Accessibility ▸ Automatically tag PDF): give an untagged document a
//! logical structure tree from its layout.
//!
//! 1. Each page's content is read into *units*: one text-showing operator, one image (an image
//!    XObject or an inline image), one form XObject, one painted path, one shading. Marked
//!    content left over from earlier tags (MCIDs with no tree to go with them) is taken out;
//!    artifacts and optional content stay as they are.
//! 2. A provisional copy wraps each unit in its own marked-content sequence and the caller's
//!    text extractor reads it back ([`AutotagText`]): each glyph's Unicode text and box in the
//!    page as displayed, with the unit it belongs to. The extractor decodes fonts; this module
//!    only does layout.
//! 3. Layout: glyphs become pieces (one baseline of a unit), pieces become lines, lines become
//!    blocks (by spacing, alignment, size and weight). Ruled tables are found from the lines a
//!    page draws (a grid of at least two rows and two columns) and their text goes to cells.
//!    Running headers and footers (the same text at the top or bottom of two or more pages,
//!    digits ignored) and page numbers become pagination artifacts; paths and shadings become
//!    layout and background artifacts.
//! 4. Roles: text larger than the body size (the size most characters use) is a heading, one
//!    level per size (H1 the largest), and a short line set wholly in a bold font at body size is
//!    a heading below those; levels never skip. Blocks starting with a bullet or a number are
//!    list items (L > LI > Lbl + LBody, the Lbl only when the marker is drawn on its own),
//!    images are figures (flagged: they need alternate text), the rest are paragraphs. Link
//!    annotations become Link elements holding the text under them and an object reference;
//!    other annotations (not widgets or popups) become Annot elements.
//! 5. Reading order: blocks are ordered by recursive cuts. Rows of blocks separated by
//!    horizontal gaps are grouped while they share a vertical gutter, and a group read column by
//!    column, so two-column pages read down the left column and then the right one.
//! 6. The rewrite: every unit goes inside a marked-content sequence (`/P <</MCID n>> BDC` …
//!    `EMC`, or `/Artifact`), consecutive units of the same element share one, and sequences
//!    open and close between whole operations, never across `q`, `Q`, `BT`, `ET`, other marked
//!    content or inside a path. The tree is Document → elements with `/Pg` and their MCIDs, the
//!    parent tree maps each page's MCIDs and each tagged annotation back, pages get
//!    `/StructParents` and `/Tabs /S`, annotations `/StructParent`, and the catalog
//!    `/MarkInfo << /Marked true >>` (and `/Lang` when given and missing).
//! 7. Form fields (`detect_fields`): every widget becomes a Form element with an object
//!    reference, next to its label in reading order, with alternate text from its tooltip, name
//!    or label; fields without a tooltip are reported with a suggestion (see `autotag_fields.rs`).
//!
//! Everything is bounded (pages, units, glyphs, rules, cells, recursion) and nothing changes
//! unless the whole document can be tagged; pages that can't be read are left untagged and
//! reported.

use std::collections::{BTreeMap, HashMap, HashSet};

use pdfcraft_content::{Matrix, Op};
use pdfcraft_cos::{Dict, Document, ObjRef, Object, PdfString, Stream};

use crate::tags::{catalog, page_content, sub_dict};

/// The most pages one call tags.
pub const MAX_AUTOTAG_PAGES: usize = 20_000;
/// Pages with more painting operations than this are left untagged.
const MAX_UNITS_PER_PAGE: usize = 100_000;
/// The whole document may have at most this many painting operations.
const MAX_UNITS: usize = 1_000_000;
/// Glyphs beyond this many on a page are not read.
const MAX_GLYPHS_PER_PAGE: usize = 400_000;
/// Ruling lines beyond this many on a page are not read.
const MAX_RULES_PER_PAGE: usize = 4_000;
/// Tables with more cells than this are read as text.
const MAX_TABLE_CELLS: usize = 10_000;
/// Annotations beyond this many on a page are not tagged.
const MAX_ANNOTS_PER_PAGE: usize = 10_000;
/// Marked content nested deeper than this leaves the page untagged.
const MAX_NESTING: usize = 4096;
/// The graphics state stack is followed this deep.
const MAX_GSTATE: usize = 4096;
/// Reading-order cuts nest at most this deep.
const MAX_CUT_DEPTH: usize = 48;
/// Characters kept per piece and per block for classification.
const MAX_PIECE_CHARS: usize = 4096;
const MAX_BLOCK_CHARS: usize = 2000;
/// Characters of a link's text kept as its description.
const MAX_LINK_CHARS: usize = 1000;
/// The language tag's longest form.
const MAX_LANG: usize = 35;

/// What to tag and how.
#[derive(Clone, Debug, PartialEq)]
pub struct AutotagOptions {
    /// 0-based pages to tag (`None`: all). A document that is already tagged is retagged as a
    /// whole, so this must be left out (or list every page) with `replace_existing`.
    pub pages: Option<Vec<usize>>,
    /// Discard the existing structure tree and tag again (otherwise a tagged document is refused).
    pub replace_existing: bool,
    /// The document language (`/Lang`), set when the catalog has none.
    pub lang: Option<String>,
    /// Find ruled tables (Table > TR > TH/TD).
    pub detect_tables: bool,
    /// Find bulleted and numbered lists (L > LI > Lbl/LBody).
    pub detect_lists: bool,
    /// Tag form fields: each widget becomes a Form element next to its label (otherwise
    /// widgets are left untagged).
    pub detect_fields: bool,
    /// What to do about fields without a tooltip (`/TU`).
    pub field_tooltips: FieldTooltips,
}

impl Default for AutotagOptions {
    fn default() -> Self {
        Self {
            pages: None,
            replace_existing: false,
            lang: None,
            detect_tables: true,
            detect_lists: true,
            detect_fields: true,
            field_tooltips: FieldTooltips::Report,
        }
    }
}

/// Fields without a tooltip: report them with a suggestion, or also write it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FieldTooltips {
    /// List them in [`AutotagReport::fields_needing_tooltip`] with a suggested tooltip.
    #[default]
    Report,
    /// Also write the suggestion as the field's `/TU` (and use it as alternate text).
    Fill,
}

/// A form field widget the autotagger tagged.
#[derive(Clone, Debug, PartialEq)]
pub struct AutotagField {
    /// 0-based page.
    pub page: usize,
    /// The widget annotation.
    pub widget: ObjRef,
    /// Its field (the widget itself when they are merged).
    pub field: ObjRef,
    /// The Form element holding it.
    pub element: ObjRef,
    /// The field's full name (`/T` of it and its ancestors, joined with dots).
    pub name: String,
    /// text, checkbox, radio, button, combo, list, signature or unknown.
    pub kind: String,
    /// Where it is, in points from the top-left of the displayed page.
    pub rect: [f64; 4],
    /// The alternate text given to the Form element.
    pub alt: Option<String>,
    /// Where it came from: tooltip, suggested (written by `fill`), name or label.
    pub alt_source: Option<String>,
    /// The label text it was placed next to.
    pub label: Option<String>,
}

/// A field without a tooltip.
#[derive(Clone, Debug, PartialEq)]
pub struct AutotagFieldTooltip {
    /// 0-based page of its first widget.
    pub page: usize,
    pub field: ObjRef,
    /// Its full name.
    pub name: String,
    /// A tooltip from its label (else its name).
    pub suggested: Option<String>,
    /// The suggestion was written as `/TU` (`fill`).
    pub filled: bool,
}

/// One glyph the text extractor found inside a provisional unit.
#[derive(Clone, Debug, PartialEq)]
pub struct AutotagGlyph {
    /// Its Unicode text.
    pub text: String,
    /// Its box `[x0, y0, x1, y1]` in points from the top-left of the page as displayed (y down).
    pub rect: [f64; 4],
    /// The marked-content id of the provisional unit around it.
    pub mcid: i64,
}

/// The text extractor: given the provisional document and 0-based page numbers, each page's
/// glyphs (in the same order), or `None` for a page whose text can't be read.
pub type AutotagText<'a> = dyn FnMut(&Document, &[usize]) -> Vec<Option<Vec<AutotagGlyph>>> + 'a;

/// A figure the autotagger made: it has no alternate text yet.
#[derive(Clone, Debug, PartialEq)]
pub struct AutotagFigure {
    /// 0-based page.
    pub page: usize,
    /// The Figure element.
    pub element: ObjRef,
    /// Where it is drawn, in points from the top-left of the displayed page.
    pub rect: [f64; 4],
}

/// What [`autotag`] did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AutotagReport {
    /// 0-based pages tagged.
    pub pages: Vec<usize>,
    /// Pages left untagged, with the reason.
    pub skipped: Vec<(usize, String)>,
    /// Structure elements made, by type (Document, H1, P, L, LI, Lbl, LBody, Table, TR, TH, TD,
    /// Figure, Link, Annot, Form).
    pub counts: BTreeMap<String, usize>,
    /// Artifact marked-content sequences written (running headers and footers, page numbers,
    /// decoration).
    pub artifacts: usize,
    /// Running headers, footers and page numbers among them (blocks of text).
    pub pagination_artifacts: usize,
    /// Figures that need alternate text (all of them: none is invented).
    pub figures_needing_alt: Vec<AutotagFigure>,
    /// Link annotations tagged (inside Link elements).
    pub links: usize,
    /// Link annotations given a description (`/Contents`) from their text or address.
    pub link_descriptions: usize,
    /// Other annotations tagged (inside Annot elements).
    pub annotations: usize,
    /// Form field widgets tagged (each in a Form element), in page order.
    pub fields: Vec<AutotagField>,
    /// Fields that have no tooltip (`/TU`), with a suggested one.
    pub fields_needing_tooltip: Vec<AutotagFieldTooltip>,
    /// Tooltips written (`fill`).
    pub tooltips_filled: usize,
    /// The language set on the catalog, when one was missing and given.
    pub lang: Option<String>,
    /// An existing structure tree was replaced.
    pub replaced: bool,
    /// Things worth knowing that did not stop the tagging.
    pub warnings: Vec<String>,
}

impl AutotagReport {
    /// Elements of one type.
    pub fn count(&self, ty: &str) -> usize {
        self.counts.get(ty).copied().unwrap_or(0)
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum AutotagError {
    #[error("the document is already tagged (it has a structure tree); autotagging would discard those tags: pass replace_existing to tag it again")]
    AlreadyTagged,
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Cos(#[from] pdfcraft_cos::CosError),
}

fn invalid(s: impl Into<String>) -> AutotagError {
    AutotagError::Invalid(s.into())
}

/// A language tag (BCP 47 shape): letters, digits and hyphens, starting with a letter.
fn valid_lang(l: &str) -> bool {
    !l.is_empty()
        && l.len() <= MAX_LANG
        && l.split('-').all(|p| !p.is_empty() && p.len() <= 8 && p.bytes().all(|b| b.is_ascii_alphanumeric()))
        && l.bytes().next().is_some_and(|b| b.is_ascii_alphabetic())
}

// ---- 1. Units ------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Text {
        bold: bool,
    },
    Image,
    /// A form XObject; `image`: its resources hold an image.
    Form {
        image: bool,
    },
    Path,
    Shading,
}

#[derive(Clone, Debug)]
struct Unit {
    kind: Kind,
    /// Its operations `start..=end`.
    start: usize,
    end: usize,
    /// Where it paints, in view space (points from the top-left, y down) and user space.
    view: Option<[f64; 4]>,
    user: Option<[f64; 4]>,
}

/// An axis-aligned ruling line in view space.
#[derive(Clone, Copy, Debug)]
struct Rule {
    horizontal: bool,
    /// y of a horizontal line, x of a vertical one.
    at: f64,
    from: f64,
    to: f64,
}

struct PageScan {
    ops: Vec<Op>,
    units: Vec<Unit>,
    unit_of: Vec<Option<u32>>,
    rules: Vec<Rule>,
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

/// Does the operator paint (or build a path)? A sequence open for another element closes
/// before it.
fn is_content(op: &[u8]) -> bool {
    matches!(op, b"Tj" | b"TJ" | b"'" | b"\"" | b"Do" | b"BI" | b"sh" | b"n") || is_path_build(op) || is_path_paint(op)
}

fn finite(r: [f64; 4]) -> Option<[f64; 4]> {
    r.iter().all(|v| v.is_finite()).then_some(r)
}

/// The marked-content properties of a BMC/BDC: inline or from the page's /Properties.
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

/// Take out marked content that carries an MCID (tags with no tree to go with them, or the
/// tree being replaced) with its EMC; drop EMCs that close nothing; artifacts keep their
/// properties without an MCID.
fn clean_ops(doc: &Document, ops: Vec<Op>, props: &Dict) -> Result<Vec<Op>, String> {
    let mut out = Vec::with_capacity(ops.len());
    let mut stack: Vec<bool> = Vec::new();
    for op in ops {
        match op.op.as_slice() {
            b"BMC" | b"BDC" => {
                if stack.len() >= MAX_NESTING {
                    return Err("its marked content is nested too deeply".into());
                }
                let p = mc_props(doc, &op, props);
                let mcid = p.as_ref().and_then(|d| d.get(b"MCID")).is_some();
                let artifact = op.name(0) == Some(b"Artifact");
                if mcid && artifact {
                    let mut d = p.unwrap_or_default();
                    d.remove(b"MCID");
                    stack.push(false);
                    out.push(Op::new("BDC", vec![Object::name("Artifact"), Object::Dict(d)]));
                } else if mcid {
                    stack.push(true);
                } else {
                    stack.push(false);
                    out.push(op);
                }
            }
            // An EMC that closes a sequence taken out, or nothing at all, goes too.
            b"EMC" => {
                if let Some(false) = stack.pop() {
                    out.push(op);
                }
            }
            _ => out.push(op),
        }
    }
    Ok(out)
}

/// Is this font bold (by name, weight or the ForceBold flag)?
fn font_bold(doc: &Document, font: &Dict) -> bool {
    let named_bold = |d: &Dict| {
        d.name(b"BaseFont").or_else(|| d.name(b"FontName")).is_some_and(|n| {
            let n = String::from_utf8_lossy(n).to_ascii_lowercase();
            let n = n.split_once('+').map_or(n.as_str(), |(_, rest)| rest).to_owned();
            ["bold", "black", "heavy", "semibold", "demi"].iter().any(|w| n.contains(w))
        })
    };
    let descriptor_bold = |d: &Dict| {
        let fd = sub_dict(doc, d, b"FontDescriptor");
        let weight = fd.get(b"FontWeight").and_then(|o| doc.resolve(o).as_f64()).unwrap_or(0.0);
        let flags = fd.get(b"Flags").and_then(|o| doc.resolve(o).as_int()).unwrap_or(0);
        weight >= 600.0 || flags & (1 << 18) != 0 || named_bold(&fd)
    };
    if named_bold(font) || descriptor_bold(font) {
        return true;
    }
    // A Type 0 font: its descendant.
    let first = font.get(b"DescendantFonts").map(|o| doc.resolve(o)).and_then(|a| a.as_array().and_then(|a| a.first().cloned()));
    first.map(|o| doc.resolve(&o)).and_then(|o| o.as_dict().cloned()).is_some_and(|d| named_bold(&d) || descriptor_bold(&d))
}

/// A path being built: its first operation, a structural operation inside it, its rules, the
/// current point and the start of the subpath.
struct PathAcc {
    start: usize,
    broken: bool,
    rules: Vec<Rule>,
    cur: (f64, f64),
    sub: (f64, f64),
}

fn add_rule(rules: &mut Vec<Rule>, a: (f64, f64), b: (f64, f64)) {
    if rules.len() >= 256 || ![a.0, a.1, b.0, b.1].iter().all(|v| v.is_finite()) {
        return;
    }
    let (dx, dy) = ((b.0 - a.0).abs(), (b.1 - a.1).abs());
    if dy < 1.0 && dx >= 8.0 {
        rules.push(Rule { horizontal: true, at: (a.1 + b.1) / 2.0, from: a.0.min(b.0), to: a.0.max(b.0) });
    } else if dx < 1.0 && dy >= 8.0 {
        rules.push(Rule { horizontal: false, at: (a.0 + b.0) / 2.0, from: a.1.min(b.1), to: a.1.max(b.1) });
    }
}

fn scan_page(doc: &Document, page: &pdfcraft_model::Page, view: &Matrix) -> Result<PageScan, String> {
    let parsed = pdfcraft_content::parse(&page_content(doc, &page.dict));
    if parsed.skipped > 0 {
        return Err("its content has bytes that can't be read, so rewriting it could lose some".into());
    }
    let res = sub_dict(doc, &page.dict, b"Resources");
    let fonts = sub_dict(doc, &res, b"Font");
    let xobjects = sub_dict(doc, &res, b"XObject");
    let props = sub_dict(doc, &res, b"Properties");
    let ops = clean_ops(doc, parsed.ops, &props)?;
    let mut units: Vec<Unit> = Vec::new();
    let mut unit_of: Vec<Option<u32>> = vec![None; ops.len()];
    let mut rules: Vec<Rule> = Vec::new();
    // Graphics state: (CTM, bold font).
    let mut gs: Vec<(Matrix, bool)> = vec![(Matrix::IDENTITY, false)];
    let mut marks: Vec<bool> = Vec::new();
    let mut path: Option<PathAcc> = None;
    let mut bold_cache: HashMap<Vec<u8>, bool> = HashMap::new();
    for (i, op) in ops.iter().enumerate() {
        let (top, bold) = gs.last().copied().unwrap_or((Matrix::IDENTITY, false));
        let to_view = top.then(view);
        let name = op.op.as_slice();
        let in_artifact = marks.iter().any(|a| *a);
        if let Some(p) = path.as_mut()
            && !is_path_build(name)
            && !is_path_paint(name)
            && name != b"n"
            && is_structural(name)
        {
            p.broken = true;
        }
        let mut unit: Option<(Kind, usize, Option<[f64; 4]>)> = None;
        match name {
            b"q" => {
                if gs.len() < MAX_GSTATE {
                    gs.push((top, bold));
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
                    t.0 = m.then(&top);
                }
            }
            b"BMC" | b"BDC" => {
                if marks.len() < MAX_NESTING {
                    marks.push(op.name(0) == Some(b"Artifact"));
                }
            }
            b"EMC" => {
                marks.pop();
            }
            b"Tf" => {
                let b = match op.name(0) {
                    Some(n) => *bold_cache.entry(n.to_vec()).or_insert_with(|| {
                        fonts.get(n).map(|f| doc.resolve(f)).and_then(|f| f.as_dict().cloned()).is_some_and(|f| font_bold(doc, &f))
                    }),
                    None => false,
                };
                if let Some(t) = gs.last_mut() {
                    t.1 = b;
                }
            }
            b"Tj" | b"TJ" | b"'" | b"\"" => unit = Some((Kind::Text { bold }, i, None)),
            b"Do" => {
                let target = op.name(0).and_then(|n| xobjects.get(n)).map(|x| doc.resolve(x));
                if let Some(Object::Stream(s)) = target.as_deref() {
                    match s.dict.name(b"Subtype") {
                        Some(b"Image") => unit = Some((Kind::Image, i, Some([0.0, 0.0, 1.0, 1.0]))),
                        Some(b"Form") => {
                            let bb: Vec<f64> = s
                                .dict
                                .get(b"BBox")
                                .map(|b| doc.resolve(b))
                                .and_then(|b| b.as_array().map(|a| a.iter().filter_map(Object::as_f64).collect()))
                                .unwrap_or_default();
                            let m = Matrix::from_operands(s.dict.get(b"Matrix").and_then(|m| m.as_array()).map(Vec::as_slice).unwrap_or(&[]))
                                .unwrap_or(Matrix::IDENTITY);
                            let r = match bb[..] {
                                [a, b, c, d] => Some(m.then(&top).bbox([a.min(c), b.min(d), a.max(c), b.max(d)])),
                                _ => None,
                            };
                            let inner = sub_dict(doc, &s.dict, b"Resources");
                            let image = sub_dict(doc, &inner, b"XObject")
                                .iter()
                                .any(|(_, x)| matches!(&*doc.resolve(x), Object::Stream(x) if x.dict.name(b"Subtype") == Some(b"Image")));
                            // The box is already in user space: converted below with the view.
                            if let Some(r) = r.and_then(finite) {
                                units_push(&mut units, &mut unit_of, Kind::Form { image }, i, i, Some(view.bbox(r)), Some(r), in_artifact)?;
                            } else {
                                units_push(&mut units, &mut unit_of, Kind::Form { image }, i, i, None, None, in_artifact)?;
                            }
                        }
                        _ => {}
                    }
                }
            }
            b"BI" => unit = Some((Kind::Image, i, Some([0.0, 0.0, 1.0, 1.0]))),
            b"sh" => unit = Some((Kind::Shading, i, None)),
            b"m" | b"l" | b"c" | b"v" | b"y" | b"re" | b"h" | b"W" | b"W*" => {
                let p = path.get_or_insert(PathAcc { start: i, broken: false, rules: Vec::new(), cur: (0.0, 0.0), sub: (0.0, 0.0) });
                let pt = |x: f64, y: f64| to_view.apply(x, y);
                match name {
                    b"m" => {
                        if let Some([x, y]) = op.nums::<2>() {
                            p.cur = pt(x, y);
                            p.sub = p.cur;
                        }
                    }
                    b"l" => {
                        if let Some([x, y]) = op.nums::<2>() {
                            let n = pt(x, y);
                            add_rule(&mut p.rules, p.cur, n);
                            p.cur = n;
                        }
                    }
                    b"c" => {
                        if let Some([_, _, _, _, x, y]) = op.nums::<6>() {
                            p.cur = pt(x, y);
                        }
                    }
                    b"v" | b"y" => {
                        if let Some([_, _, x, y]) = op.nums::<4>() {
                            p.cur = pt(x, y);
                        }
                    }
                    b"re" => {
                        if let Some([x, y, w, h]) = op.nums::<4>() {
                            let c = [pt(x, y), pt(x + w, y), pt(x + w, y + h), pt(x, y + h)];
                            for k in 0..4 {
                                if let (Some(a), Some(b)) = (c.get(k), c.get((k + 1) % 4)) {
                                    add_rule(&mut p.rules, *a, *b);
                                }
                            }
                            p.cur = pt(x, y);
                            p.sub = p.cur;
                        }
                    }
                    b"h" => {
                        add_rule(&mut p.rules, p.cur, p.sub);
                        p.cur = p.sub;
                    }
                    _ => {}
                }
            }
            b"n" => path = None,
            _ if is_path_paint(name) => match path.take() {
                Some(p) if !p.broken => {
                    if !in_artifact && rules.len() < MAX_RULES_PER_PAGE {
                        rules.extend(p.rules);
                    }
                    units_push(&mut units, &mut unit_of, Kind::Path, p.start, i, None, None, in_artifact)?;
                }
                // Painting across a structural operator can't be wrapped as a whole.
                Some(_) => {}
                None => units_push(&mut units, &mut unit_of, Kind::Path, i, i, None, None, in_artifact)?,
            },
            _ => {}
        }
        if let Some((kind, start, unit_box)) = unit {
            let user = unit_box.map(|b| top.bbox(b)).and_then(finite);
            let v = unit_box.map(|b| to_view.bbox(b)).and_then(finite);
            units_push(&mut units, &mut unit_of, kind, start, i, v, user, in_artifact)?;
        }
    }
    Ok(PageScan { ops, units, unit_of, rules })
}

#[allow(clippy::too_many_arguments)]
fn units_push(
    units: &mut Vec<Unit>,
    unit_of: &mut [Option<u32>],
    kind: Kind,
    start: usize,
    end: usize,
    view: Option<[f64; 4]>,
    user: Option<[f64; 4]>,
    in_artifact: bool,
) -> Result<(), String> {
    // Content already marked as an artifact stays as it is.
    if in_artifact {
        return Ok(());
    }
    if units.len() >= MAX_UNITS_PER_PAGE {
        return Err(format!("it has more than {MAX_UNITS_PER_PAGE} painting operations"));
    }
    let id = u32::try_from(units.len()).map_err(|_| "it has too many painting operations".to_string())?;
    for slot in unit_of.iter_mut().take(end.saturating_add(1)).skip(start) {
        *slot = Some(id);
    }
    units.push(Unit { kind, start, end, view, user });
    Ok(())
}

/// The provisional content: each unit in its own `/Span <</MCID unit>>` sequence.
fn provisional_ops(s: &PageScan) -> Vec<Op> {
    let mut out = Vec::with_capacity(s.ops.len().saturating_add(s.units.len().saturating_mul(2)));
    for (i, op) in s.ops.iter().enumerate() {
        let unit = s.unit_of.get(i).copied().flatten().and_then(|u| s.units.get(u as usize).map(|x| (u, x)));
        if let Some((u, x)) = unit
            && x.start == i
        {
            let mut d = Dict::new();
            d.set(b"MCID".to_vec(), Object::Int(i64::from(u)));
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

// ---- 3. Layout -----------------------------------------------------------------------------

/// One baseline of a unit's glyphs.
#[derive(Clone, Debug)]
struct Piece {
    unit: u32,
    rect: [f64; 4],
    size: f64,
    text: String,
    bold: bool,
}

fn cx(r: &[f64; 4]) -> f64 {
    (r[0] + r[2]) / 2.0
}

fn cy(r: &[f64; 4]) -> f64 {
    (r[1] + r[3]) / 2.0
}

fn union(a: &mut [f64; 4], b: &[f64; 4]) {
    a[0] = a[0].min(b[0]);
    a[1] = a[1].min(b[1]);
    a[2] = a[2].max(b[2]);
    a[3] = a[3].max(b[3]);
}

fn norm(r: [f64; 4]) -> [f64; 4] {
    [r[0].min(r[2]), r[1].min(r[3]), r[0].max(r[2]), r[1].max(r[3])]
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(1.0)
}

/// Join texts in left-to-right order, with a space where they stand apart or have one at
/// their edge. Runs of whitespace become one space; a space at either end is kept (so pieces
/// drawn apart join with one).
fn join_text(items: &[(f64, f64, &str)], size: f64, max: usize) -> String {
    let mut s = String::new();
    let mut last: Option<f64> = None;
    let mut chars = 0usize;
    let mut pending = false;
    let mut lead = false;
    for (x0, x1, t) in items {
        let core = t.trim();
        if core.is_empty() {
            pending = true;
            last = Some(*x1);
            continue;
        }
        if t.starts_with(char::is_whitespace) || last.is_some_and(|l| x0 - l > size * 0.15) {
            pending = true;
        }
        if pending {
            if s.is_empty() {
                lead = true;
            } else {
                s.push(' ');
            }
        }
        let mut space = false;
        for c in core.chars() {
            if chars >= max {
                break;
            }
            if c.is_whitespace() {
                space = true;
                continue;
            }
            if space {
                s.push(' ');
                space = false;
            }
            s.push(c);
            chars += 1;
        }
        pending = t.ends_with(char::is_whitespace);
        last = Some(*x1);
    }
    if s.is_empty() {
        return s;
    }
    if pending {
        s.push(' ');
    }
    if lead {
        s.insert(0, ' ');
    }
    s
}

/// Glyphs on one baseline: centre y, height, (box, text).
type Baseline<'a> = (f64, f64, Vec<([f64; 4], &'a str)>);

fn pieces_of(scan: &PageScan, glyphs: &[AutotagGlyph]) -> Vec<Piece> {
    let mut by_unit: BTreeMap<u32, Vec<([f64; 4], &str)>> = BTreeMap::new();
    for g in glyphs.iter().take(MAX_GLYPHS_PER_PAGE) {
        let Some(r) = finite(norm(g.rect)) else { continue };
        if r[2] - r[0] > 10_000.0 || r[3] - r[1] > 10_000.0 {
            continue;
        }
        let Ok(u) = u32::try_from(g.mcid) else { continue };
        let Some(unit) = scan.units.get(u as usize) else { continue };
        if !matches!(unit.kind, Kind::Text { .. } | Kind::Form { .. }) {
            continue;
        }
        by_unit.entry(u).or_default().push((r, g.text.as_str()));
    }
    let mut out = Vec::new();
    for (u, gs) in by_unit {
        let bold = matches!(scan.units.get(u as usize).map(|x| x.kind), Some(Kind::Text { bold: true }));
        // Baselines, in the order the glyphs come.
        let mut groups: Vec<Baseline<'_>> = Vec::new();
        for (r, t) in gs {
            let h = (r[3] - r[1]).max(0.1);
            let c = cy(&r);
            match groups.iter_mut().rev().take(8).find(|(gc, gh, _)| (gc - c).abs() < 0.5 * gh.min(h)) {
                Some(g) => g.2.push((r, t)),
                None => {
                    if groups.len() >= 4096 {
                        continue;
                    }
                    groups.push((c, h, vec![(r, t)]));
                }
            }
        }
        for (_, _, mut g) in groups {
            g.sort_by(|a, b| a.0[0].total_cmp(&b.0[0]));
            let mut heights: Vec<f64> = g.iter().map(|(r, _)| (r[3] - r[1]).max(0.1)).collect();
            let size = median(&mut heights);
            let Some(first) = g.first() else { continue };
            let mut rect = first.0;
            for (r, _) in &g {
                union(&mut rect, r);
            }
            let items: Vec<(f64, f64, &str)> = g.iter().map(|(r, t)| (r[0], r[2], *t)).collect();
            let text = join_text(&items, size, MAX_PIECE_CHARS);
            if text.trim().is_empty() {
                continue;
            }
            out.push(Piece { unit: u, rect, size, text, bold });
        }
    }
    out
}

#[derive(Clone, Debug)]
struct Line {
    pieces: Vec<usize>,
    rect: [f64; 4],
    size: f64,
    bold: bool,
    text: String,
}

fn lines_of(pieces: &[Piece], members: &[usize]) -> Vec<Line> {
    let mut idx: Vec<usize> = members.to_vec();
    idx.sort_by(|a, b| {
        let (ra, rb) = (pieces.get(*a).map_or(0.0, |p| cy(&p.rect)), pieces.get(*b).map_or(0.0, |p| cy(&p.rect)));
        ra.total_cmp(&rb)
    });
    let mut lines: Vec<Line> = Vec::new();
    for i in idx {
        let Some(p) = pieces.get(i) else { continue };
        let c = cy(&p.rect);
        let mut target = None;
        for (k, l) in lines.iter().enumerate().rev() {
            let lc = cy(&l.rect);
            if c - lc > p.size.max(l.size) * 2.0 {
                break;
            }
            let s = p.size.max(l.size);
            let gap = (p.rect[0] - l.rect[2]).max(l.rect[0] - p.rect[2]);
            if (c - lc).abs() < 0.4 * s && gap < 0.9 * s {
                target = Some(k);
                break;
            }
        }
        match target.and_then(|k| lines.get_mut(k)) {
            Some(l) => {
                l.pieces.push(i);
                union(&mut l.rect, &p.rect);
                l.size = l.size.max(p.size);
                l.bold &= p.bold;
            }
            None => lines.push(Line { pieces: vec![i], rect: p.rect, size: p.size, bold: p.bold, text: String::new() }),
        }
    }
    for l in &mut lines {
        l.pieces.sort_by(|a, b| pieces.get(*a).map_or(0.0, |p| p.rect[0]).total_cmp(&pieces.get(*b).map_or(0.0, |p| p.rect[0])));
        let items: Vec<(f64, f64, &str)> = l.pieces.iter().filter_map(|i| pieces.get(*i)).map(|p| (p.rect[0], p.rect[2], p.text.as_str())).collect();
        l.text = join_text(&items, l.size, MAX_BLOCK_CHARS);
    }
    lines
}

/// The byte length of a list marker at the start of `text` (a bullet, or a number or letter
/// followed by `.` or `)`), when the text has one followed by a space or nothing.
fn list_marker(text: &str) -> Option<usize> {
    let t = text.trim_start();
    let lead = text.len() - t.len();
    let mut chars = t.char_indices();
    let (_, first) = chars.next()?;
    let ends = |at: usize| t.get(at..).is_some_and(|r| r.is_empty() || r.starts_with(char::is_whitespace));
    if "•◦▪▫‣⁃●○■□–—-*·►▸✓✔➢❖".contains(first) {
        let at = first.len_utf8();
        return ends(at).then_some(lead + at);
    }
    // (1) / (a)
    let body = t.strip_prefix('(').unwrap_or(t);
    let opened = body.len() != t.len();
    let digits: usize = body.bytes().take_while(u8::is_ascii_digit).count();
    let letters: usize = body.bytes().take_while(u8::is_ascii_alphabetic).count();
    let n = if (1..=3).contains(&digits) {
        digits
    } else if letters == 1 || (letters <= 5 && body.bytes().take(letters).all(|b| b"ivxlcIVXLC".contains(&b))) {
        letters
    } else {
        return None;
    };
    if n == 0 {
        return None;
    }
    let close = body.as_bytes().get(n).copied()?;
    let ok = if opened { close == b')' } else { close == b'.' || close == b')' };
    let at = (t.len() - body.len()) + n + 1;
    (ok && ends(at)).then_some(lead + at)
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Role {
    P,
    Heading(usize),
    Item,
    Figure,
    Table,
}

#[derive(Clone, Debug)]
enum BlockKind {
    Text(Vec<Line>),
    Figure(u32),
    Table(usize),
}

#[derive(Clone, Debug)]
struct Block {
    kind: BlockKind,
    rect: [f64; 4],
    size: f64,
    bold: bool,
    text: String,
    /// The first line starts with a list marker.
    marker: bool,
    artifact: Option<Art>,
    role: Role,
}

impl Block {
    fn lines(&self) -> &[Line] {
        match &self.kind {
            BlockKind::Text(l) => l,
            _ => &[],
        }
    }
}

fn blocks_of(lines: Vec<Line>, lists: bool) -> Vec<Block> {
    let mut lines = lines;
    lines.sort_by(|a, b| a.rect[1].total_cmp(&b.rect[1]));
    let mut blocks: Vec<(Vec<Line>, [f64; 4], bool)> = Vec::new();
    let mut active: Vec<usize> = Vec::new();
    for line in lines {
        let marker = lists && list_marker(&line.text).is_some();
        active.retain(|b| blocks.get(*b).and_then(|(ls, _, _)| ls.last()).is_some_and(|l| line.rect[1] - l.rect[3] < 4.0 * l.size.max(line.size)));
        let mut best: Option<(usize, f64)> = None;
        if !marker {
            for &b in &active {
                let Some((ls, rect, is_item)) = blocks.get(b) else { continue };
                let (Some(last), Some(first)) = (ls.last(), ls.first()) else { continue };
                let s = last.size.max(0.1);
                let gap = line.rect[1] - last.rect[3];
                let overlap = line.rect[2].min(rect[2]) - line.rect[0].max(rect[0]);
                let narrower = (line.rect[2] - line.rect[0]).min(rect[2] - rect[0]).max(1.0);
                let ok = gap > -0.35 * s
                    && gap < 0.55 * s
                    && (line.size / s - 1.0).abs() < 0.12
                    && line.bold == last.bold
                    && overlap > 0.3 * narrower
                    && (!is_item || line.rect[0] > first.rect[0] + 0.3 * s);
                if ok && best.is_none_or(|(_, g)| gap.abs() < g) {
                    best = Some((b, gap.abs()));
                }
            }
        }
        match best.and_then(|(b, _)| blocks.get_mut(b)) {
            Some((ls, rect, _)) => {
                union(rect, &line.rect);
                ls.push(line);
            }
            None => {
                active.push(blocks.len());
                let r = line.rect;
                blocks.push((vec![line], r, marker));
            }
        }
    }
    blocks
        .into_iter()
        .map(|(ls, rect, marker)| {
            let size = ls.first().map_or(1.0, |l| l.size);
            let bold = ls.iter().all(|l| l.bold);
            let mut text = String::new();
            for l in &ls {
                if text.len() >= MAX_BLOCK_CHARS {
                    break;
                }
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(l.text.trim());
            }
            Block { kind: BlockKind::Text(ls), rect, size, bold, text, marker, artifact: None, role: Role::P }
        })
        .collect()
}

/// A ruled table: its grid (view space) and the pieces in each cell, row by row.
#[derive(Clone, Debug)]
struct Table {
    rect: [f64; 4],
    rows: usize,
    cols: usize,
    cells: Vec<Vec<usize>>,
}

/// Merge values closer than `tol` (sorted, each cluster's mean).
fn cluster(mut v: Vec<f64>, tol: f64) -> Vec<f64> {
    v.retain(|x| x.is_finite());
    v.sort_by(f64::total_cmp);
    let mut out: Vec<(f64, usize)> = Vec::new();
    for x in v {
        match out.last_mut() {
            Some((sum, n)) if x - *sum / *n as f64 <= tol => {
                *sum += x;
                *n += 1;
            }
            _ => out.push((x, 1)),
        }
    }
    out.into_iter().map(|(s, n)| s / n as f64).collect()
}

/// Collinear rules merged: same orientation, `at` within 2pt, overlapping or touching.
fn merge_rules(rules: &[Rule], horizontal: bool) -> Vec<Rule> {
    let mut v: Vec<Rule> = rules.iter().filter(|r| r.horizontal == horizontal).copied().collect();
    v.sort_by(|a, b| a.at.total_cmp(&b.at).then(a.from.total_cmp(&b.from)));
    let mut out: Vec<Rule> = Vec::new();
    for r in v {
        if let Some(last) = out.iter_mut().rev().take(16).find(|l| (l.at - r.at).abs() <= 2.0 && r.from <= l.to + 2.0 && r.to >= l.from - 2.0) {
            last.from = last.from.min(r.from);
            last.to = last.to.max(r.to);
            continue;
        }
        out.push(r);
    }
    out
}

fn find(parent: &mut [usize], i: usize) -> usize {
    let mut r = i;
    let mut steps = 0usize;
    while let Some(&p) = parent.get(r) {
        if p == r || steps > parent.len() {
            break;
        }
        r = p;
        steps += 1;
    }
    // Path compression, bounded the same way.
    let mut c = i;
    for _ in 0..steps {
        let Some(&p) = parent.get(c) else { break };
        if let Some(slot) = parent.get_mut(c) {
            *slot = r;
        }
        c = p;
    }
    r
}

/// Grids drawn by ruling lines: at least three horizontal and three vertical lines that meet.
/// A connected set of rules: their ys, xs and box.
type Grid = (Vec<f64>, Vec<f64>, [f64; 4]);

fn grids(rules: &[Rule]) -> Vec<([f64; 4], Vec<f64>, Vec<f64>)> {
    let h = merge_rules(rules, true);
    let v = merge_rules(rules, false);
    if h.len() < 3 || v.len() < 3 {
        return Vec::new();
    }
    let n = h.len().saturating_add(v.len());
    let mut parent: Vec<usize> = (0..n).collect();
    for (i, a) in h.iter().enumerate() {
        for (j, b) in v.iter().enumerate() {
            let meets = b.at >= a.from - 2.0 && b.at <= a.to + 2.0 && a.at >= b.from - 2.0 && a.at <= b.to + 2.0;
            if meets {
                let (ri, rj) = (find(&mut parent, i), find(&mut parent, h.len() + j));
                if ri != rj
                    && let Some(slot) = parent.get_mut(ri)
                {
                    *slot = rj;
                }
            }
        }
    }
    let mut comps: BTreeMap<usize, Grid> = BTreeMap::new();
    for i in 0..n {
        let root = find(&mut parent, i);
        let (rect, is_h, at) = match (h.get(i), i.checked_sub(h.len()).and_then(|j| v.get(j))) {
            (Some(r), _) => ([r.from, r.at, r.to, r.at], true, r.at),
            (None, Some(r)) => ([r.at, r.from, r.at, r.to], false, r.at),
            _ => continue,
        };
        let e = comps.entry(root).or_insert_with(|| (Vec::new(), Vec::new(), rect));
        if is_h {
            e.0.push(at);
        } else {
            e.1.push(at);
        }
        union(&mut e.2, &rect);
    }
    comps
        .into_values()
        .filter_map(|(ys, xs, rect)| {
            let (ys, xs) = (cluster(ys, 4.0), cluster(xs, 4.0));
            let cells = ys.len().saturating_sub(1).saturating_mul(xs.len().saturating_sub(1));
            (ys.len() >= 3 && xs.len() >= 3 && cells <= MAX_TABLE_CELLS && rect[2] - rect[0] >= 20.0 && rect[3] - rect[1] >= 20.0)
                .then_some((rect, ys, xs))
        })
        .collect()
}

/// Which slot between sorted boundaries `b` holds `x`.
fn slot(b: &[f64], x: f64) -> Option<usize> {
    b.windows(2).position(|w| matches!(w, [lo, hi] if x >= *lo && x < *hi))
}

/// Tables from the page's rules, taking the pieces inside their cells (`taken`).
fn tables_of(rules: &[Rule], pieces: &[Piece], taken: &mut [bool]) -> Vec<Table> {
    let mut out = Vec::new();
    for (rect, ys, xs) in grids(rules) {
        let (rows, cols) = (ys.len().saturating_sub(1), xs.len().saturating_sub(1));
        let mut cells: Vec<Vec<usize>> = vec![Vec::new(); rows.saturating_mul(cols)];
        let mut inside: Vec<usize> = Vec::new();
        for (i, p) in pieces.iter().enumerate() {
            if taken.get(i).copied().unwrap_or(true) {
                continue;
            }
            let (x, y) = (cx(&p.rect), cy(&p.rect));
            if let (Some(r), Some(c)) = (slot(&ys, y), slot(&xs, x))
                && let Some(cell) = cells.get_mut(r.saturating_mul(cols).saturating_add(c))
            {
                cell.push(i);
                inside.push(i);
            }
        }
        // Rows and columns with nothing in them (double rules, spacers) are dropped.
        let keep_row: Vec<bool> = (0..rows).map(|r| (0..cols).any(|c| cells.get(r * cols + c).is_some_and(|x| !x.is_empty()))).collect();
        let keep_col: Vec<bool> = (0..cols).map(|c| (0..rows).any(|r| cells.get(r * cols + c).is_some_and(|x| !x.is_empty()))).collect();
        let filled = cells.iter().filter(|c| !c.is_empty()).count();
        let (kr, kc) = (keep_row.iter().filter(|k| **k).count(), keep_col.iter().filter(|k| **k).count());
        if filled < 2 || kr == 0 || kc == 0 {
            continue;
        }
        let mut kept = Vec::with_capacity(kr.saturating_mul(kc));
        for (r, kr) in keep_row.iter().enumerate() {
            for (c, kc) in keep_col.iter().enumerate() {
                if *kr && *kc {
                    let mut cell = cells.get(r * cols + c).cloned().unwrap_or_default();
                    cell.sort_by(|a, b| {
                        let (pa, pb) = (pieces.get(*a), pieces.get(*b));
                        let key = |p: Option<&Piece>| p.map_or((0.0, 0.0), |p| (p.rect[1], p.rect[0]));
                        let (ka, kb) = (key(pa), key(pb));
                        ka.0.total_cmp(&kb.0).then(ka.1.total_cmp(&kb.1))
                    });
                    kept.push(cell);
                }
            }
        }
        for i in inside {
            if let Some(t) = taken.get_mut(i) {
                *t = true;
            }
        }
        out.push(Table { rect, rows: kr, cols: kc, cells: kept });
    }
    out
}

// ---- 5. Reading order ----------------------------------------------------------------------

/// Gaps of at least this many points between blocks count as a gutter.
const MIN_GUTTER: f64 = 4.0;

/// The vertical gaps (x intervals no block covers) within a set of blocks.
fn gaps(rects: &[[f64; 4]], items: &[usize]) -> Vec<(f64, f64)> {
    let mut spans: Vec<(f64, f64)> = items.iter().filter_map(|i| rects.get(*i)).map(|r| (r[0], r[2])).collect();
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out = Vec::new();
    let mut reach: Option<f64> = None;
    for (a, b) in spans {
        if let Some(r) = reach {
            if a - r >= MIN_GUTTER {
                out.push((r, a));
            }
            reach = Some(r.max(b));
        } else {
            reach = Some(b);
        }
    }
    out
}

fn crosses(rects: &[[f64; 4]], items: &[usize], g: (f64, f64)) -> bool {
    items.iter().filter_map(|i| rects.get(*i)).any(|r| r[0] < g.1 - 0.5 && r[2] > g.0 + 0.5)
}

fn by_position(rects: &[[f64; 4]], mut items: Vec<usize>) -> Vec<usize> {
    items.sort_by(|a, b| {
        let (ra, rb) = (rects.get(*a).copied().unwrap_or_default(), rects.get(*b).copied().unwrap_or_default());
        ra[1].total_cmp(&rb[1]).then(ra[0].total_cmp(&rb[0]))
    });
    items
}

/// Reading order of blocks: rows separated by horizontal gaps, rows that share a gutter read
/// column by column, recursively.
/// Bands that share a gutter: their blocks, the gutter, how many bands.
type Group = (Vec<usize>, Option<(f64, f64)>, usize);

fn reading_order(rects: &[[f64; 4]], items: Vec<usize>, depth: usize) -> Vec<usize> {
    if items.len() <= 1 || depth >= MAX_CUT_DEPTH {
        return by_position(rects, items);
    }
    // Bands: blocks that overlap vertically.
    let sorted = by_position(rects, items);
    let mut bands: Vec<(Vec<usize>, f64)> = Vec::new();
    for i in sorted {
        let Some(r) = rects.get(i) else { continue };
        match bands.last_mut() {
            Some((b, bottom)) if r[1] < *bottom - 0.1 => {
                b.push(i);
                *bottom = bottom.max(r[3]);
            }
            _ => bands.push((vec![i], r[3])),
        }
    }
    // Groups of bands that share a gutter.
    let mut groups: Vec<Group> = Vec::new();
    for (band, _) in bands {
        let band_gaps = gaps(rects, &band);
        if let Some((members, Some(g), n)) = groups.last_mut() {
            let shared = band_gaps.iter().find_map(|x| {
                let (a, b) = (x.0.max(g.0), x.1.min(g.1));
                (b - a >= MIN_GUTTER).then_some((a, b))
            });
            if let Some(ng) = shared {
                *g = ng;
                members.extend(band);
                *n += 1;
                continue;
            }
            if !crosses(rects, &band, *g) {
                members.extend(band);
                *n += 1;
                continue;
            }
        }
        let widest = band_gaps.iter().copied().max_by(|a, b| (a.1 - a.0).total_cmp(&(b.1 - b.0)));
        match widest {
            Some(g) => {
                // Blocks just above that sit within one column belong to the group.
                let mut members: Vec<usize> = Vec::new();
                if let Some((prev, None, _)) = groups.last_mut() {
                    let mut split = prev.len();
                    while split > 0 {
                        let Some(i) = prev.get(split - 1) else { break };
                        if crosses(rects, &[*i], g) {
                            break;
                        }
                        split -= 1;
                    }
                    members = prev.split_off(split);
                    if prev.is_empty() {
                        groups.pop();
                    }
                }
                members.extend(band);
                groups.push((members, Some(g), 1));
            }
            None => match groups.last_mut() {
                Some((members, None, n)) => {
                    members.extend(band);
                    *n += 1;
                }
                _ => groups.push((band, None, 1)),
            },
        }
    }
    let mut out = Vec::new();
    let total = groups.len();
    for (members, gutter, _) in groups {
        match gutter {
            Some(g) => {
                let mid = (g.0 + g.1) / 2.0;
                let (left, right): (Vec<usize>, Vec<usize>) = members.iter().partition(|i| rects.get(**i).is_some_and(|r| cx(r) < mid));
                if left.is_empty() || right.is_empty() {
                    out.extend(by_position(rects, members));
                } else {
                    out.extend(reading_order(rects, left, depth + 1));
                    out.extend(reading_order(rects, right, depth + 1));
                }
            }
            None if total > 1 => out.extend(reading_order(rects, members, depth + 1)),
            None => out.extend(by_position(rects, members)),
        }
    }
    out
}

// ---- 4. Roles ------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Art {
    /// No properties (text with no readable characters).
    Plain,
    Layout,
    Background,
    Header,
    Footer,
}

impl Art {
    fn opening(self) -> Op {
        let props = |ty: &str, sub: Option<&str>| {
            let mut d = Dict::new();
            d.set(b"Type".to_vec(), Object::name(ty));
            if let Some(s) = sub {
                d.set(b"Subtype".to_vec(), Object::name(s));
            }
            Op::new("BDC", vec![Object::name("Artifact"), Object::Dict(d)])
        };
        match self {
            Art::Plain => Op::new("BMC", vec![Object::name("Artifact")]),
            Art::Layout => props("Layout", None),
            Art::Background => props("Background", None),
            Art::Header => props("Pagination", Some("Header")),
            Art::Footer => props("Pagination", Some("Footer")),
        }
    }
}

/// Text as compared across pages: lower case, digits as `#`, single spaces.
fn running_key(text: &str) -> String {
    let mut s = String::new();
    for w in text.split_whitespace() {
        if !s.is_empty() {
            s.push(' ');
        }
        s.extend(w.chars().take(200).map(|c| if c.is_ascii_digit() { '#' } else { c.to_ascii_lowercase() }));
        if s.len() > 400 {
            break;
        }
    }
    s
}

/// A page number: `7`, `- 7 -`, `Page 7`, `7 of 12`, `7/12`, `vii`.
fn is_page_number(text: &str) -> bool {
    let t = text.trim().trim_matches(|c: char| c == '-' || c == '–' || c == '—' || c == '|' || c.is_whitespace()).to_ascii_lowercase();
    let t = t.strip_prefix("page").map(str::trim).unwrap_or(&t).to_owned();
    if t.is_empty() || t.len() > 24 {
        return false;
    }
    let numberish = |s: &str| {
        let s = s.trim();
        (!s.is_empty() && s.len() <= 6 && s.bytes().all(|b| b.is_ascii_digit()))
            || (!s.is_empty() && s.len() <= 8 && s.bytes().all(|b| b"ivxlc".contains(&b)))
    };
    if numberish(&t) {
        return true;
    }
    for sep in [" of ", "/"] {
        if let Some((a, b)) = t.split_once(sep) {
            return numberish(a) && numberish(b);
        }
    }
    false
}

/// The size most characters are set in (rounded to half points).
fn body_size(layouts: &[Layout]) -> f64 {
    let mut hist: BTreeMap<i64, usize> = BTreeMap::new();
    for l in layouts {
        for b in &l.blocks {
            if b.artifact.is_some() || !matches!(b.kind, BlockKind::Text(_)) {
                continue;
            }
            let key = (b.size * 2.0).round();
            if key.is_finite() && key.abs() < 1e9 {
                *hist.entry(key as i64).or_default() += b.text.chars().count();
            }
        }
    }
    hist.into_iter().max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0))).map_or(10.0, |(k, _)| (k as f64 / 2.0).max(0.5))
}

// ---- Layout of one page --------------------------------------------------------------------

struct Layout {
    page: usize,
    obj: ObjRef,
    /// User space → view space.
    view: Matrix,
    height: f64,
    scan: PageScan,
    pieces: Vec<Piece>,
    blocks: Vec<Block>,
    tables: Vec<Table>,
    /// Blocks in reading order (artifacts left out).
    order: Vec<usize>,
}

fn layout_page(page: usize, obj: ObjRef, view: Matrix, height: f64, scan: PageScan, glyphs: &[AutotagGlyph], options: &AutotagOptions) -> Layout {
    let pieces = pieces_of(&scan, glyphs);
    let mut taken = vec![false; pieces.len()];
    let tables = if options.detect_tables { tables_of(&scan.rules, &pieces, &mut taken) } else { Vec::new() };
    let free: Vec<usize> = (0..pieces.len()).filter(|i| !taken.get(*i).copied().unwrap_or(true)).collect();
    let mut blocks = blocks_of(lines_of(&pieces, &free), options.detect_lists);
    for (t, table) in tables.iter().enumerate() {
        blocks.push(Block {
            kind: BlockKind::Table(t),
            rect: table.rect,
            size: 0.0,
            bold: false,
            text: String::new(),
            marker: false,
            artifact: None,
            role: Role::Table,
        });
    }
    // Figures: images, and forms that draw an image and no text.
    let with_text: HashSet<u32> = pieces.iter().map(|p| p.unit).collect();
    for (u, unit) in scan.units.iter().enumerate() {
        let Ok(u) = u32::try_from(u) else { break };
        let figure = match unit.kind {
            Kind::Image => true,
            Kind::Form { image } => image && !with_text.contains(&u),
            _ => false,
        };
        if figure {
            let rect = unit.view.unwrap_or_default();
            blocks.push(Block {
                kind: BlockKind::Figure(u),
                rect,
                size: 0.0,
                bold: false,
                text: String::new(),
                marker: false,
                artifact: None,
                role: Role::Figure,
            });
        }
    }
    Layout { page, obj, view, height, scan, pieces, blocks, tables, order: Vec::new() }
}

/// The share of the page height at the top and bottom where running headers and footers sit.
const HEADER_BAND: f64 = 0.08;

/// Running headers and footers: text (or an image) at the top or bottom of the page that comes
/// back on another page, and page numbers.
fn mark_running(layouts: &mut [Layout]) {
    let band = |l: &Layout, r: &[f64; 4]| {
        let h = l.height.max(1.0);
        if r[3] <= h * HEADER_BAND {
            Some(Art::Header)
        } else if r[1] >= h * (1.0 - HEADER_BAND) {
            Some(Art::Footer)
        } else {
            None
        }
    };
    let key = |b: &Block| match &b.kind {
        BlockKind::Text(_) => Some(running_key(&b.text)),
        BlockKind::Figure(_) => Some(format!("image {:.0} {:.0} {:.0} {:.0}", b.rect[0], b.rect[1], b.rect[2], b.rect[3])),
        BlockKind::Table(_) => None,
    };
    let mut seen: HashMap<(bool, String), HashSet<usize>> = HashMap::new();
    for l in layouts.iter() {
        for b in &l.blocks {
            if let (Some(a), Some(k)) = (band(l, &b.rect), key(b)) {
                seen.entry((a == Art::Header, k)).or_default().insert(l.page);
            }
        }
    }
    for l in layouts.iter_mut() {
        let bands: Vec<Option<Art>> = l.blocks.iter().map(|b| band(l, &b.rect)).collect();
        for (b, a) in l.blocks.iter_mut().zip(bands) {
            let (Some(a), Some(k)) = (a, key(b)) else { continue };
            let repeated = seen.get(&(a == Art::Header, k)).is_some_and(|p| p.len() >= 2);
            let number = matches!(b.kind, BlockKind::Text(_)) && is_page_number(&b.text);
            if repeated || number {
                b.artifact = Some(a);
            }
        }
    }
}

/// Give each text block its role; headings get a provisional level (fixed in reading order
/// later).
fn classify(layouts: &mut [Layout], lists: bool) {
    let body = body_size(layouts);
    let is_heading_size =
        |b: &Block| b.lines().len() <= 3 && b.text.chars().count() <= 200 && b.size >= body * 1.15 && b.text.chars().any(char::is_alphanumeric);
    let mut sizes: Vec<f64> = Vec::new();
    for l in layouts.iter() {
        for b in &l.blocks {
            if b.artifact.is_none() && matches!(b.kind, BlockKind::Text(_)) && is_heading_size(b) {
                sizes.push((b.size * 2.0).round() / 2.0);
            }
        }
    }
    let mut sizes = cluster(sizes, 0.6);
    sizes.reverse();
    for l in layouts.iter_mut() {
        for b in &mut l.blocks {
            if b.artifact.is_some() || !matches!(b.kind, BlockKind::Text(_)) {
                continue;
            }
            let chars = b.text.chars().count();
            b.role = if is_heading_size(b) {
                let rounded = (b.size * 2.0).round() / 2.0;
                let rank = sizes.iter().position(|s| (s - rounded).abs() <= 0.6).unwrap_or(sizes.len());
                Role::Heading((rank + 1).min(6))
            } else if lists && b.marker {
                Role::Item
            } else if b.bold
                && b.lines().len() == 1
                && (2..=120).contains(&chars)
                && b.text.chars().any(char::is_alphabetic)
                && !b.text.ends_with('.')
            {
                Role::Heading((sizes.len() + 1).min(6))
            } else {
                Role::P
            };
        }
    }
}

// ---- 6. The tree ---------------------------------------------------------------------------

/// Sort key of an element's kids: (page, block in reading order, then position inside it).
type Rank = (usize, usize, usize, usize);

#[derive(Clone, Debug)]
enum Kid {
    Mcid { page: usize, mcid: i64 },
    Elem(usize),
    Objr { obj: ObjRef, page: usize },
}

#[derive(Clone, Debug)]
struct Elem {
    ty: String,
    parent: Option<usize>,
    page: Option<usize>,
    kids: Vec<(Rank, Kid)>,
    attrs: Option<Dict>,
    /// User-space box of a figure.
    bbox: Option<[f64; 4]>,
    /// View-space box of a figure.
    view: Option<[f64; 4]>,
    /// Alternate text (a form field's).
    alt: Option<String>,
}

#[derive(Default)]
struct Tree {
    elems: Vec<Elem>,
}

impl Tree {
    fn add(&mut self, ty: &str, parent: usize, page: usize, rank: Rank) -> usize {
        let i = self.elems.len();
        self.elems.push(Elem {
            ty: ty.into(),
            parent: Some(parent),
            page: Some(page),
            kids: Vec::new(),
            attrs: None,
            bbox: None,
            view: None,
            alt: None,
        });
        if let Some(p) = self.elems.get_mut(parent) {
            p.kids.push((rank, Kid::Elem(i)));
        }
        i
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    None,
    Elem(usize),
    Art(Art),
}

fn attr(owner: &str, key: &str, value: Object) -> Dict {
    let mut d = Dict::new();
    d.set(b"O".to_vec(), Object::name(owner));
    d.set(key.as_bytes().to_vec(), value);
    d
}

/// A page's annotation to tag.
struct Annot {
    obj: ObjRef,
    elem: usize,
    /// A description to write when it has none.
    contents: Option<String>,
}

/// Build the elements of one page and the target of each of its units.
struct PagePlan {
    targets: Vec<Target>,
    ranks: Vec<Rank>,
    annots: Vec<Annot>,
    /// Annotations whose old /StructParent goes (replacing a tree).
    untag: Vec<ObjRef>,
}

fn plan_page(
    doc: &Document,
    pg: &pdfcraft_model::Page,
    l: &Layout,
    tree: &mut Tree,
    options: &AutotagOptions,
    field_state: &mut fields::FieldState,
    report: &mut AutotagReport,
) -> PagePlan {
    let n = l.scan.units.len();
    let mut targets = vec![Target::None; n];
    let mut ranks: Vec<Rank> = vec![(l.page, usize::MAX, 0, 0); n];
    // Units with text, decided by the first of their pieces in reading order.
    fn set(u: u32, t: Target, r: Rank, targets: &mut [Target], ranks: &mut [Rank]) {
        let u = u as usize;
        if let (Some(slot), Some(rk)) = (targets.get_mut(u), ranks.get_mut(u))
            && (*slot == Target::None || r < *rk)
        {
            *slot = t;
            *rk = r;
        }
    }
    let page = l.page;
    let mut list: Option<(usize, f64)> = None;
    // Where each piece of text went (labels place the form fields).
    let mut home: Vec<Option<fields::Home>> = vec![None; l.pieces.len()];
    for (pos, &b) in l.order.iter().enumerate() {
        let Some(block) = l.blocks.get(b) else { continue };
        let rank = (page, pos, 0, 0);
        if block.role != Role::Item {
            list = None;
        }
        match (&block.kind, block.role) {
            (BlockKind::Figure(u), _) => {
                let e = tree.add("Figure", 0, page, rank);
                if let Some(el) = tree.elems.get_mut(e) {
                    let user = l.scan.units.get(*u as usize).and_then(|x| x.user);
                    if let Some(bb) = user {
                        el.attrs = Some(attr("Layout", "BBox", Object::Array(bb.iter().map(|v| Object::Real(*v)).collect())));
                    }
                    el.bbox = user;
                    el.view = Some(block.rect);
                }
                set(*u, Target::Elem(e), rank, &mut targets, &mut ranks);
            }
            (BlockKind::Table(t), _) => {
                let Some(table) = l.tables.get(*t) else { continue };
                let te = tree.add("Table", 0, page, rank);
                for r in 0..table.rows {
                    let tr = tree.add("TR", te, page, (page, pos, r, 0));
                    for c in 0..table.cols {
                        let header = r == 0;
                        let cell = tree.add(if header { "TH" } else { "TD" }, tr, page, (page, pos, r, c));
                        if header && let Some(el) = tree.elems.get_mut(cell) {
                            el.attrs = Some(attr("Table", "Scope", Object::name("Column")));
                        }
                        let members = table.cells.get(r.saturating_mul(table.cols).saturating_add(c)).cloned().unwrap_or_default();
                        for (k, p) in members.iter().enumerate() {
                            if let Some(piece) = l.pieces.get(*p) {
                                let rk = (page, pos, r.saturating_mul(table.cols).saturating_add(c), k);
                                set(piece.unit, Target::Elem(cell), rk, &mut targets, &mut ranks);
                                if let Some(h) = home.get_mut(*p) {
                                    *h = Some(fields::Home { elem: cell, rank: rk, heading: false });
                                }
                            }
                        }
                    }
                }
            }
            (BlockKind::Text(lines), role) => {
                let container = match role {
                    Role::Item => {
                        let x = block.rect[0];
                        let l_elem = match list {
                            Some((le, lx)) if (x - lx).abs() <= block.size.max(1.0) * 1.5 => le,
                            _ => tree.add("L", 0, page, rank),
                        };
                        list = Some((l_elem, x));
                        let li = tree.add("LI", l_elem, page, rank);
                        // The marker gets a Lbl when it is drawn on its own.
                        let first = lines.first().and_then(|ln| ln.pieces.first()).and_then(|p| l.pieces.get(*p));
                        let alone = first.is_some_and(|p| {
                            list_marker(&p.text).is_some_and(|m| p.text.trim().len() <= m)
                                && !l.pieces.iter().any(|q| q.unit == p.unit && !std::ptr::eq(q, p))
                        });
                        if alone && let Some(p) = first {
                            let lbl = tree.add("Lbl", li, page, (page, pos, 0, 0));
                            set(p.unit, Target::Elem(lbl), (page, pos, 0, 0), &mut targets, &mut ranks);
                        }
                        tree.add("LBody", li, page, (page, pos, 1, 0))
                    }
                    Role::Heading(level) => tree.add(&format!("H{}", level.clamp(1, 6)), 0, page, rank),
                    _ => tree.add("P", 0, page, rank),
                };
                let heading = matches!(role, Role::Heading(_));
                for (li, line) in lines.iter().enumerate() {
                    for (k, p) in line.pieces.iter().enumerate() {
                        if let Some(piece) = l.pieces.get(*p) {
                            let rk = (page, pos, li.saturating_add(1), k);
                            set(piece.unit, Target::Elem(container), rk, &mut targets, &mut ranks);
                            if let Some(h) = home.get_mut(*p) {
                                *h = Some(fields::Home { elem: container, rank: rk, heading });
                            }
                        }
                    }
                }
            }
        }
    }
    // Pagination artifacts.
    for b in &l.blocks {
        let Some(a) = b.artifact else { continue };
        report.pagination_artifacts = report.pagination_artifacts.saturating_add(1);
        match &b.kind {
            BlockKind::Text(lines) => {
                for line in lines {
                    for p in &line.pieces {
                        if let Some(piece) = l.pieces.get(*p)
                            && let Some(slot) = targets.get_mut(piece.unit as usize)
                            && *slot == Target::None
                        {
                            *slot = Target::Art(a);
                        }
                    }
                }
            }
            BlockKind::Figure(u) => {
                if let Some(slot) = targets.get_mut(*u as usize) {
                    *slot = Target::Art(a);
                }
            }
            BlockKind::Table(_) => {}
        }
    }
    // Everything else is decoration.
    for (u, unit) in l.scan.units.iter().enumerate() {
        let Some(slot) = targets.get_mut(u) else { continue };
        if *slot != Target::None {
            continue;
        }
        *slot = Target::Art(match unit.kind {
            Kind::Shading => Art::Background,
            Kind::Text { .. } => Art::Plain,
            _ => Art::Layout,
        });
    }

    // Annotations.
    let mut annots = Vec::new();
    let mut untag = Vec::new();
    let mut in_link: HashSet<u32> = HashSet::new();
    let annot_list = pg.dict.get(b"Annots").map(|o| doc.resolve(o)).and_then(|a| a.as_array().cloned()).unwrap_or_default();
    let mut loose: Vec<([f64; 4], ObjRef, &'static str, Option<String>)> = Vec::new();
    let mut inline = 0usize;
    let mut widgets: Vec<(ObjRef, Dict)> = Vec::new();
    for a in annot_list.iter().take(MAX_ANNOTS_PER_PAGE) {
        let Some(r) = a.as_ref() else {
            if a.as_dict().is_some() {
                inline += 1;
            }
            continue;
        };
        let Some(d) = doc.get(r).as_dict().cloned() else { continue };
        let subtype = d.name(b"Subtype").unwrap_or(b"");
        if subtype == b"Widget" && options.detect_fields {
            widgets.push((r, d));
            continue;
        }
        if matches!(subtype, b"Popup" | b"Widget" | b"PrinterMark" | b"TrapNet") {
            if d.contains(b"StructParent") {
                untag.push(r);
            }
            continue;
        }
        let rect: Vec<f64> = d
            .get(b"Rect")
            .map(|o| doc.resolve(o))
            .and_then(|o| o.as_array().map(|a| a.iter().filter_map(Object::as_f64).collect()))
            .unwrap_or_default();
        let view = match rect[..] {
            [a, b, c, e] => finite(l.view.bbox(norm([a, b, c, e]))),
            _ => None,
        };
        let has_contents =
            d.get(b"Contents").map(|o| doc.resolve(o)).and_then(|o| o.as_string().map(|s| !s.to_text().trim().is_empty())).unwrap_or(false);
        if subtype != b"Link" {
            report.annotations = report.annotations.saturating_add(1);
            loose.push((view.unwrap_or_default(), r, "Annot", None));
            continue;
        }
        report.links = report.links.saturating_add(1);
        // The text under the link: pieces whose centre is inside it.
        let mut under: Vec<(Rank, u32, usize)> = Vec::new();
        if let Some(v) = view {
            for (i, p) in l.pieces.iter().enumerate() {
                let (x, y) = (cx(&p.rect), cy(&p.rect));
                if x >= v[0] - 1.0 && x <= v[2] + 1.0 && y >= v[1] - 1.0 && y <= v[3] + 1.0 && !in_link.contains(&p.unit) {
                    let u = p.unit as usize;
                    if let (Some(Target::Elem(_)), Some(rk)) = (targets.get(u), ranks.get(u)) {
                        under.push((*rk, p.unit, i));
                    }
                }
            }
        }
        under.sort_by_key(|a| a.0);
        let uri = || {
            let action = sub_dict(doc, &d, b"A");
            action.get(b"URI").map(|o| doc.resolve(o)).and_then(|o| o.as_string().map(PdfString::to_text))
        };
        let Some(&(first_rank, first_unit, _)) = under.first() else {
            let contents = if has_contents { None } else { uri() };
            loose.push((view.unwrap_or_default(), r, "Link", contents));
            continue;
        };
        let Some(Target::Elem(container)) = targets.get(first_unit as usize).copied() else { continue };
        let link = tree.add("Link", container, page, first_rank);
        let mut text = String::new();
        for (_, u, i) in &under {
            if targets.get(*u as usize) != Some(&Target::Elem(container)) && !in_link.contains(u) {
                continue;
            }
            if in_link.insert(*u)
                && let Some(t) = targets.get_mut(*u as usize)
            {
                *t = Target::Elem(link);
            }
            if let Some(p) = l.pieces.get(*i)
                && text.chars().count() < MAX_LINK_CHARS
            {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.extend(p.text.trim().chars().take(MAX_LINK_CHARS));
            }
        }
        if let Some(el) = tree.elems.get_mut(link) {
            el.kids.push(((page, usize::MAX, usize::MAX, usize::MAX), Kid::Objr { obj: r, page }));
        }
        let contents = if has_contents { None } else { Some(text).filter(|t| !t.trim().is_empty()).or_else(uri) };
        annots.push(Annot { obj: r, elem: link, contents });
    }
    if inline > 0 {
        report.warnings.push(format!(
            "page {}: {inline} annotation(s) written inside the page's /Annots array can't be referenced from the tags and were left untagged",
            page.saturating_add(1)
        ));
    }
    // Form fields next to their labels.
    if !widgets.is_empty() {
        fields::tag_widgets(doc, l, widgets, &home, tree, options.field_tooltips, field_state, &mut annots, &mut untag);
    }
    // Annotations with no text under them go after the page's blocks, top to bottom.
    loose.sort_by(|a, b| a.0[1].total_cmp(&b.0[1]).then(a.0[0].total_cmp(&b.0[0])));
    let base = l.order.len();
    for (k, (_, r, ty, contents)) in loose.into_iter().enumerate() {
        let rank = (page, base.saturating_add(k), 0, 0);
        let e = tree.add(ty, 0, page, rank);
        if let Some(el) = tree.elems.get_mut(e) {
            el.kids.push((rank, Kid::Objr { obj: r, page }));
        }
        annots.push(Annot { obj: r, elem: e, contents });
    }
    PagePlan { targets, ranks, annots, untag }
}

/// The page's content with every unit in its marked content: the ops, and for each MCID (in
/// order) the element it belongs to. Consecutive units of one element share a sequence.
fn emit(l: &Layout, plan: &PagePlan, tree: &mut Tree, report: &mut AutotagReport) -> (Vec<Op>, Vec<usize>) {
    let s = &l.scan;
    let mut out = Vec::with_capacity(s.ops.len().saturating_add(s.units.len().saturating_mul(2)));
    let mut mcids: Vec<usize> = Vec::new();
    let mut open: Option<Target> = None;
    for (i, op) in s.ops.iter().enumerate() {
        let unit = s.unit_of.get(i).copied().flatten();
        let target = unit.and_then(|u| plan.targets.get(u as usize).copied()).unwrap_or(Target::None);
        let name = op.op.as_slice();
        match target {
            Target::None => {
                if open.is_some() && (is_structural(name) || is_content(name)) {
                    out.push(Op::new("EMC", Vec::new()));
                    open = None;
                }
            }
            t => {
                if open != Some(t) {
                    if open.is_some() {
                        out.push(Op::new("EMC", Vec::new()));
                    }
                    match t {
                        Target::Elem(e) => {
                            let mcid = i64::try_from(mcids.len()).unwrap_or(i64::MAX);
                            let rank = unit.and_then(|u| plan.ranks.get(u as usize).copied()).unwrap_or((l.page, usize::MAX, 0, 0));
                            let ty = tree.elems.get(e).map_or("Span", |el| el.ty.as_str()).to_owned();
                            if let Some(el) = tree.elems.get_mut(e) {
                                el.kids.push((rank, Kid::Mcid { page: l.page, mcid }));
                            }
                            mcids.push(e);
                            let mut d = Dict::new();
                            d.set(b"MCID".to_vec(), Object::Int(mcid));
                            out.push(Op::new("BDC", vec![Object::name(&ty), Object::Dict(d)]));
                        }
                        Target::Art(a) => {
                            report.artifacts = report.artifacts.saturating_add(1);
                            out.push(a.opening());
                        }
                        Target::None => {}
                    }
                    open = Some(t);
                }
            }
        }
        out.push(op.clone());
    }
    if open.is_some() {
        out.push(Op::new("EMC", Vec::new()));
    }
    (out, mcids)
}

// ---- The edit ------------------------------------------------------------------------------

/// User space → view space (points from the top-left of the displayed page, y down), and the
/// displayed height.
pub(crate) fn view_of(doc: &Document, page: &pdfcraft_model::Page) -> (Matrix, f64) {
    let to_user = Matrix(page.view_matrix(doc));
    let (_, h) = page.display_size(doc);
    let flip = Matrix([1.0, 0.0, 0.0, -1.0, 0.0, h]);
    (to_user.invert().unwrap_or(Matrix::IDENTITY).then(&flip), h)
}

/// Tag the document (see the module docs). `text` reads the provisional pages' glyphs.
pub fn autotag(doc: &mut Document, options: &AutotagOptions, text: &mut AutotagText<'_>) -> Result<AutotagReport, AutotagError> {
    let lang = match options.lang.as_deref().map(str::trim).filter(|l| !l.is_empty()) {
        Some(l) if !valid_lang(l) => return Err(invalid(format!("{l:?} is not a language tag (for example en-US)"))),
        other => other.map(str::to_owned),
    };
    let (cat_ref, cat) = catalog(doc).ok_or_else(|| invalid("the document has no catalog to tag"))?;
    let tagged = cat.contains(b"StructTreeRoot");
    if tagged && !options.replace_existing {
        return Err(AutotagError::AlreadyTagged);
    }
    let pages = pdfcraft_model::pages(doc);
    if pages.is_empty() {
        return Err(invalid("the document has no pages to tag"));
    }
    let targets: Vec<usize> = match &options.pages {
        None => (0..pages.len()).collect(),
        Some(list) => {
            if list.is_empty() {
                return Err(invalid("pages must list at least one page"));
            }
            if let Some(p) = list.iter().find(|p| **p >= pages.len()) {
                return Err(invalid(format!("there is no page {}: the document has {} pages", p.saturating_add(1), pages.len())));
            }
            let mut v = list.clone();
            v.sort_unstable();
            v.dedup();
            v
        }
    };
    if tagged && targets.len() != pages.len() {
        return Err(invalid("replacing the tags retags the whole document: leave out pages (or list them all)"));
    }
    if targets.len() > MAX_AUTOTAG_PAGES {
        return Err(invalid(format!("tag at most {MAX_AUTOTAG_PAGES} pages at once")));
    }
    let mut report = AutotagReport { replaced: tagged, ..Default::default() };

    // 1. Units.
    let mut scans: Vec<(usize, ObjRef, Matrix, f64, PageScan)> = Vec::new();
    let mut seen_pages = HashSet::new();
    let mut total = 0usize;
    for &p in &targets {
        let Some(pg) = pages.get(p) else { continue };
        if !seen_pages.insert(pg.obj) {
            report.skipped.push((p, "the same page object appears earlier in the page tree".into()));
            continue;
        }
        let (view, height) = view_of(doc, pg);
        match scan_page(doc, pg, &view) {
            Ok(s) => {
                total = total.saturating_add(s.units.len());
                if total > MAX_UNITS {
                    return Err(invalid(format!("the document has more than {MAX_UNITS} painting operations to tag")));
                }
                scans.push((p, pg.obj, view, height, s));
            }
            Err(why) => report.skipped.push((p, why)),
        }
    }

    // 2. The provisional copy, read back by the caller's text extractor.
    let mut provisional = doc.clone();
    for (_, obj, _, _, s) in &scans {
        let st = provisional.add(Object::Stream(Stream::flate(Dict::new(), &pdfcraft_content::serialize_ops(&provisional_ops(s)))));
        provisional.update_dict(*obj, |d| d.set(b"Contents".to_vec(), Object::Ref(st)))?;
    }
    let list: Vec<usize> = scans.iter().map(|(p, ..)| *p).collect();
    let glyphs = text(&provisional, &list);
    drop(provisional);

    // 3. Layout.
    let mut layouts: Vec<Layout> = Vec::new();
    for (k, (p, obj, view, height, s)) in scans.into_iter().enumerate() {
        let Some(Some(g)) = glyphs.get(k) else {
            report.skipped.push((p, "its text could not be read".into()));
            continue;
        };
        layouts.push(layout_page(p, obj, view, height, s, g, options));
    }
    drop(glyphs);
    if layouts.is_empty() {
        return Err(invalid(match report.skipped.first() {
            Some((p, why)) => format!("no page could be tagged: page {} was not, as {why}", p.saturating_add(1)),
            None => "no page could be tagged".into(),
        }));
    }

    // 4. Roles and order.
    mark_running(&mut layouts);
    classify(&mut layouts, options.detect_lists);
    for l in &mut layouts {
        let rects: Vec<[f64; 4]> = l.blocks.iter().map(|b| b.rect).collect();
        let items: Vec<usize> = l.blocks.iter().enumerate().filter(|(_, b)| b.artifact.is_none()).map(|(i, _)| i).collect();
        l.order = reading_order(&rects, items, 0);
    }
    // Heading levels never skip, in reading order across the document.
    let mut previous = 0usize;
    for l in &mut layouts {
        for &b in &l.order {
            if let Some(block) = l.blocks.get_mut(b)
                && let Role::Heading(level) = block.role
            {
                let fixed = level.clamp(1, previous.saturating_add(1)).min(6);
                block.role = Role::Heading(fixed);
                previous = fixed;
            }
        }
    }

    // 5. Elements and the rewritten pages.
    let mut tree = Tree::default();
    tree.elems.push(Elem { ty: "Document".into(), parent: None, page: None, kids: Vec::new(), attrs: None, bbox: None, view: None, alt: None });
    struct Done {
        page: usize,
        obj: ObjRef,
        ops: Vec<Op>,
        mcids: Vec<usize>,
        annots: Vec<Annot>,
        untag: Vec<ObjRef>,
    }
    let mut done: Vec<Done> = Vec::new();
    let mut field_state = fields::FieldState::default();
    for l in &layouts {
        let Some(pg) = pages.get(l.page) else { continue };
        let plan = plan_page(doc, pg, l, &mut tree, options, &mut field_state, &mut report);
        let (ops, mcids) = emit(l, &plan, &mut tree, &mut report);
        done.push(Done { page: l.page, obj: l.obj, ops, mcids, annots: plan.annots, untag: plan.untag });
    }
    drop(layouts);

    // Elements left with nothing in them go (a block whose text belongs to another's unit).
    let prunable = |ty: &str| matches!(ty, "P" | "H1" | "H2" | "H3" | "H4" | "H5" | "H6" | "Lbl" | "LBody" | "LI" | "L" | "Figure" | "Link");
    let mut alive = vec![true; tree.elems.len()];
    for _ in 0..8 {
        let mut changed = false;
        for i in 1..tree.elems.len() {
            let Some(el) = tree.elems.get(i) else { continue };
            let live_kids = el.kids.iter().filter(|(_, k)| !matches!(k, Kid::Elem(e) if !alive.get(*e).copied().unwrap_or(false))).count();
            if alive.get(i).copied().unwrap_or(false) && live_kids == 0 && prunable(&el.ty) {
                if let Some(a) = alive.get_mut(i) {
                    *a = false;
                }
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // 6. Write it.
    let root_ref = doc.add(Object::Null);
    let pt_ref = doc.add(Object::Null);
    let refs: Vec<Option<ObjRef>> = alive.iter().map(|a| a.then(|| doc.add(Object::Null))).collect();
    let page_refs: HashMap<usize, ObjRef> = pages.iter().enumerate().map(|(i, p)| (i, p.obj)).collect();
    for (i, el) in tree.elems.iter_mut().enumerate() {
        let Some(Some(r)) = refs.get(i) else { continue };
        el.kids.sort_by_key(|a| a.0);
        let mut d = Dict::new();
        d.set(b"Type".to_vec(), Object::name("StructElem"));
        d.set(b"S".to_vec(), Object::name(&el.ty));
        let parent = match el.parent {
            Some(p) => refs.get(p).copied().flatten().unwrap_or(root_ref),
            None => root_ref,
        };
        d.set(b"P".to_vec(), Object::Ref(parent));
        // The element's page: that of its first marked content, else its own.
        let first_page = el.kids.iter().find_map(|(_, k)| match k {
            Kid::Mcid { page, .. } => Some(*page),
            _ => None,
        });
        let pg = first_page.or(el.page);
        if let Some(pr) = pg.and_then(|p| page_refs.get(&p)) {
            d.set(b"Pg".to_vec(), Object::Ref(*pr));
        }
        let mut kids = Vec::with_capacity(el.kids.len());
        for (_, k) in &el.kids {
            match k {
                Kid::Mcid { page, mcid } if Some(*page) == pg => kids.push(Object::Int(*mcid)),
                Kid::Mcid { page, mcid } => {
                    let mut m = Dict::new();
                    m.set(b"Type".to_vec(), Object::name("MCR"));
                    if let Some(pr) = page_refs.get(page) {
                        m.set(b"Pg".to_vec(), Object::Ref(*pr));
                    }
                    m.set(b"MCID".to_vec(), Object::Int(*mcid));
                    kids.push(Object::Dict(m));
                }
                Kid::Elem(e) => {
                    if let Some(Some(er)) = refs.get(*e) {
                        kids.push(Object::Ref(*er));
                    }
                }
                Kid::Objr { obj, page } => {
                    let mut o = Dict::new();
                    o.set(b"Type".to_vec(), Object::name("OBJR"));
                    if let Some(pr) = page_refs.get(page) {
                        o.set(b"Pg".to_vec(), Object::Ref(*pr));
                    }
                    o.set(b"Obj".to_vec(), Object::Ref(*obj));
                    kids.push(Object::Dict(o));
                }
            }
        }
        d.set(b"K".to_vec(), Object::Array(kids));
        if let Some(a) = &el.attrs {
            d.set(b"A".to_vec(), Object::Dict(a.clone()));
        }
        if let Some(alt) = &el.alt {
            d.set(b"Alt".to_vec(), Object::String(PdfString::text(alt)));
        }
        doc.set(*r, Object::Dict(d));
        *report.counts.entry(el.ty.clone()).or_default() += 1;
        if el.ty == "Figure"
            && let Some(page) = el.page
        {
            report.figures_needing_alt.push(AutotagFigure { page, element: *r, rect: el.view.or(el.bbox).unwrap_or_default() });
        }
    }
    report.figures_needing_alt.sort_by(|a, b| a.page.cmp(&b.page).then(a.element.num.cmp(&b.element.num)));
    for f in std::mem::take(&mut field_state.pending) {
        let Some(Some(element)) = refs.get(f.elem).copied() else { continue };
        report.fields.push(AutotagField {
            page: f.page,
            widget: f.widget,
            field: f.field,
            element,
            name: f.name,
            kind: f.kind.into(),
            rect: f.rect,
            alt: f.alt,
            alt_source: f.alt_source.map(str::to_owned),
            label: f.label,
        });
    }
    report.fields_needing_tooltip = std::mem::take(&mut field_state.needing);
    for (field, tu) in &field_state.fills {
        doc.update_dict(*field, |d| d.set(b"TU".to_vec(), Object::String(PdfString::text(tu))))?;
        report.tooltips_filled = report.tooltips_filled.saturating_add(1);
    }
    if field_state.over_limit > 0 {
        report.warnings.push(format!("{} form field widget(s) beyond the first {} were left untagged", field_state.over_limit, fields::MAX_FIELDS));
    }
    for p in &field_state.unlabelled_pages {
        report.warnings.push(format!(
            "page {}: too many form fields and text pieces to look for labels; its fields were placed by position only",
            p.saturating_add(1)
        ));
    }
    if !report.fields_needing_tooltip.is_empty() && options.field_tooltips == FieldTooltips::Report {
        report.warnings.push(format!(
            "{} form field(s) have no tooltip (TU): see fields_needing_tooltip, or pass field_tooltips fill to write the suggestions",
            report.fields_needing_tooltip.len()
        ));
    }

    // The parent tree: pages first, then annotations.
    let mut nums: Vec<Object> = Vec::new();
    let mut key: i64 = 0;
    let mut annot_entries: Vec<(ObjRef, i64, Option<String>, ObjRef)> = Vec::new();
    let mut tagged_pages: HashSet<ObjRef> = HashSet::new();
    let mut untagged_pages: Vec<ObjRef> = Vec::new();
    for d in &done {
        let st = doc.add(Object::Stream(Stream::flate(Dict::new(), &pdfcraft_content::serialize_ops(&d.ops))));
        let has = !d.mcids.is_empty();
        let page_key = key;
        if has {
            nums.push(Object::Int(page_key));
            nums.push(Object::Array(d.mcids.iter().map(|e| refs.get(*e).copied().flatten().map_or(Object::Null, Object::Ref)).collect()));
            key = key.saturating_add(1);
        }
        doc.update_dict(d.obj, |p| {
            p.set(b"Contents".to_vec(), Object::Ref(st));
            p.set(b"Tabs".to_vec(), Object::name("S"));
            if has {
                p.set(b"StructParents".to_vec(), Object::Int(page_key));
            } else {
                p.remove(b"StructParents");
            }
        })?;
        tagged_pages.insert(d.obj);
        report.pages.push(d.page);
        for a in &d.annots {
            if let Some(Some(er)) = refs.get(a.elem) {
                annot_entries.push((a.obj, 0, a.contents.clone(), *er));
            }
        }
        for u in &d.untag {
            doc.update_dict(*u, |a| {
                a.remove(b"StructParent");
            })?;
        }
    }
    for (_, k, _, _) in &mut annot_entries {
        *k = key;
        key = key.saturating_add(1);
    }
    for (obj, k, contents, er) in &annot_entries {
        nums.push(Object::Int(*k));
        nums.push(Object::Ref(*er));
        doc.update_dict(*obj, |a| {
            a.set(b"StructParent".to_vec(), Object::Int(*k));
            if let Some(c) = contents {
                a.set(b"Contents".to_vec(), Object::String(PdfString::text(c)));
            }
        })?;
        if contents.is_some() {
            report.link_descriptions = report.link_descriptions.saturating_add(1);
        }
    }
    // Replacing a tree: pages left untagged (unreadable) lose their links into the old one.
    if tagged {
        for p in &pages {
            if !tagged_pages.contains(&p.obj) && p.dict.contains(b"StructParents") {
                untagged_pages.push(p.obj);
            }
        }
        for p in untagged_pages {
            doc.update_dict(p, |d| {
                d.remove(b"StructParents");
            })?;
        }
    }
    doc.set(pt_ref, {
        let mut d = Dict::new();
        d.set(b"Nums".to_vec(), Object::Array(nums));
        Object::Dict(d)
    });
    let document = refs.first().copied().flatten();
    doc.set(root_ref, {
        let mut d = Dict::new();
        d.set(b"Type".to_vec(), Object::name("StructTreeRoot"));
        d.set(b"K".to_vec(), Object::Array(document.map(Object::Ref).into_iter().collect()));
        d.set(b"ParentTree".to_vec(), Object::Ref(pt_ref));
        d.set(b"ParentTreeNextKey".to_vec(), Object::Int(key));
        Object::Dict(d)
    });
    let mut mark_info = sub_dict(doc, &cat, b"MarkInfo");
    mark_info.set(b"Marked".to_vec(), Object::Bool(true));
    mark_info.remove(b"Suspects");
    let has_lang = cat.get(b"Lang").map(|o| doc.resolve(o)).and_then(|o| o.as_string().map(|s| !s.to_text().trim().is_empty())).unwrap_or(false);
    let set_lang = if has_lang { None } else { lang };
    if !has_lang && set_lang.is_none() {
        report.warnings.push("the document has no language: give lang (for example en-US) so screen readers pick the right voice".into());
    }
    doc.update_dict(cat_ref, |c| {
        c.set(b"StructTreeRoot".to_vec(), Object::Ref(root_ref));
        c.set(b"MarkInfo".to_vec(), Object::Dict(mark_info));
        if let Some(l) = &set_lang {
            c.set(b"Lang".to_vec(), Object::String(PdfString::text(l)));
        }
    })?;
    report.lang = set_lang;
    for (p, why) in &report.skipped {
        report.warnings.push(format!("page {} was left untagged: {why}", p.saturating_add(1)));
    }
    Ok(report)
}

#[path = "autotag_fields.rs"]
mod fields;

#[cfg(test)]
#[path = "autotag_tests.rs"]
mod tests;
