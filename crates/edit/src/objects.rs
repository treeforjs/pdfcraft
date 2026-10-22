//! Edit a PDF ▸ selected page objects: Arrange (Bring to Front, Bring Forward, Send Backward,
//! Send to Back), Align (left, horizontal centre, right, top, vertical centre, bottom),
//! Distribute (horizontally, vertically), Rotate / Flip / Move, Resize, Delete, and the
//! appearance of vector paths (fill and stroke colour, line width, dash, cap, join, opacity).
//!
//! **Objects.** [`page_objects`] lists what the page's own content draws, in paint order: each
//! text object (`BT … ET`), painted path, image or Form XObject (`Do`), inline image and shading
//! (`sh`). In tagged content, everything inside one marked-content sequence with an `/MCID` is one
//! object, so a structure element's content is never split (its MCID stays unique). Content that
//! PdfCraft keeps as separate, tagged streams (added text and images, headers, watermarks, …) is
//! not listed: those items are edited with their own tools.
//!
//! **Arrange** rewrites the page's original content (the untagged streams) as one stream in the
//! new order, each object wrapped in `q … Q` and preceded by the graphics state it was painted
//! with (the `cm`s, clips, colours, line and text state, `gs` in effect, replayed in their
//! original order), so every object looks exactly as before, wherever it now sits. Marked
//! content (tags, optional content, artifacts) is reopened around each object as needed and
//! sequences that stay together stay one sequence; empty sequences and marked points keep their
//! place. Untouched objects keep their bytes; only the wrapper is new.
//!
//! **Align, distribute, rotate, flip and move** change only the selected objects, in place: each
//! is wrapped in `q <cm> … Q`, the `cm` adding a user-space transform after its own, followed by
//! the state the object left behind for later content (text state and colours set inside a text
//! object, a path's clip), so nothing else moves or changes. Everything else is copied byte for
//! byte. Directions are as displayed (after `/Rotate`). **Resize** is the same, with a scale
//! about an anchor of each object's displayed box.
//!
//! **Style** (vector paths only) wraps each selected painted path in `q <state> … Q`: an opacity
//! ExtGState (`/CA` and `/ca`, a new page resource), line width / dash / cap / join (widths and
//! dash lengths in displayed points, divided by the path's CTM scale), and fill / stroke colours
//! in DeviceGray, DeviceRGB or DeviceCMYK, or in the colour space the path already paints with
//! (`scn`, components checked against that space). Turning fill or stroke on or off changes the
//! painting operator (`f` → `B`, `B` → `S`, …); a path left painting nothing is refused. In a
//! tagged object of several kinds only its paths change. **Delete** removes the selected
//! objects' painting operators and keeps the state they leave for later content (a clip, text
//! state), so nothing else changes; keep-tags-on-edit then drops emptied structure elements.
//! Opacity alone also applies to text, images, Form XObjects and shadings: the object is wrapped
//! whole in `q /PCOpacity… gs … Q`.
//!
//! **Edit object tool.** Each [`PageObject`] reports how it is painted ([`Appearance`]: fill and
//! stroke colour, displayed line width, opacity), [`ObjectsEdit::RotateBy`] turns objects by any
//! angle, [`ObjectsEdit::Matrix`] scales or turns several about their combined box, and [`ObjectIndex`] answers clicks and marquees from a grid built once per page edit.

use std::collections::HashMap;
use std::ops::Range;

use pdfcraft_content::{Matrix, Op, Pieces, write_op};
use pdfcraft_cos::{Dict, Document, Object, Stream};

use crate::EditError;
use crate::text::{content_streams, rewritten_contents};

#[cfg(test)]
mod tests;

/// The most state operators a page's objects may need replayed in total before Arrange refuses
/// the page as too complex (each object carries its own copy).
const MAX_REPLAYED: usize = 4_000_000;
/// The most state operators kept at one `q` level.
const MAX_LEVEL: usize = 50_000;

/// What a page object is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    Text,
    Path,
    Image,
    /// A Form XObject (grouped artwork), moved and arranged as a whole.
    Form,
    InlineImage,
    Shading,
    /// Tagged content of several kinds (one marked-content sequence with an MCID).
    Group,
}

impl ObjectKind {
    pub fn label(self) -> &'static str {
        match self {
            ObjectKind::Text => "text",
            ObjectKind::Path => "path",
            ObjectKind::Image => "image",
            ObjectKind::Form => "form",
            ObjectKind::InlineImage => "inline_image",
            ObjectKind::Shading => "shading",
            ObjectKind::Group => "group",
        }
    }
}

/// An object the page's content draws.
#[derive(Clone, Debug, PartialEq)]
pub struct PageObject {
    pub kind: ObjectKind,
    /// Its box in user space; `None` when it has none that can be worked out (text whose font
    /// can't be measured): such objects can be arranged but not aligned or turned. A shading's
    /// box is the clip it fills, within the crop box.
    pub rect: Option<[f64; 4]>,
    /// For text: what it shows.
    pub text: String,
    /// The marked-content id that makes it one tagged object.
    pub mcid: Option<i64>,
    /// For a vector path (or the first path of a tagged group): how it is painted.
    pub appearance: Option<Appearance>,
    /// The constant opacity it is painted with (`/ca`, or `/CA` for a path that only strokes).
    pub opacity: f64,
}

/// How a vector path is painted, as Edit PDF's object properties show it.
#[derive(Clone, Debug, PartialEq)]
pub struct Appearance {
    /// The fill colour ([`Paint::None`]: not filled; [`Paint::Current`] with no components: a
    /// pattern).
    pub fill: Paint,
    pub stroke: Paint,
    /// The line width in displayed points.
    pub line_width: f64,
    pub fill_opacity: f64,
    pub stroke_opacity: f64,
}

/// A change of paint order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrange {
    /// Above everything else.
    Front,
    /// Above the next object up that overlaps it (or the next one up when none overlaps).
    Forward,
    /// Below the next object down that overlaps it (or the next one down when none overlaps).
    Backward,
    /// Below everything else.
    Back,
}

/// An alignment, in display space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignTo {
    Left,
    /// The centres line up on one vertical line.
    HorizontalCenter,
    Right,
    Top,
    /// The centres line up on one horizontal line.
    VerticalCenter,
    Bottom,
}

/// A distribution axis, in display space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// A colour for a path's fill or stroke.
#[derive(Clone, Debug, PartialEq)]
pub enum Paint {
    /// No fill (or no stroke): the painting operator stops filling (stroking).
    None,
    Gray(f64),
    Rgb([f64; 3]),
    Cmyk([f64; 4]),
    /// Components in the colour space the path already paints with (spot, ICC, Lab, Indexed…).
    Current(Vec<f64>),
}

/// A dash pattern in displayed points: dashes and gaps (empty: solid), and the phase.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dash {
    pub pattern: Vec<f64>,
    pub phase: f64,
}

/// Line cap style (`J`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineCap {
    Butt,
    Round,
    Square,
}

/// Line join style (`j`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineJoin {
    Miter,
    Round,
    Bevel,
}

/// The appearance of vector paths; `None` fields are left as they are.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PathStyle {
    pub fill: Option<Paint>,
    pub stroke: Option<Paint>,
    /// Displayed points (0: the thinnest line the device can draw).
    pub line_width: Option<f64>,
    pub dash: Option<Dash>,
    pub cap: Option<LineCap>,
    pub join: Option<LineJoin>,
    /// 0 (transparent) to 1, for fill and stroke alike.
    pub opacity: Option<f64>,
}

/// The point of an object's displayed box that stays put when it is resized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    Center,
    TopLeft,
    Top,
    TopRight,
    Left,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl Anchor {
    /// Fractions of the box from its left and bottom edges.
    fn fractions(self) -> (f64, f64) {
        match self {
            Anchor::Center => (0.5, 0.5),
            Anchor::TopLeft => (0.0, 1.0),
            Anchor::Top => (0.5, 1.0),
            Anchor::TopRight => (1.0, 1.0),
            Anchor::Left => (0.0, 0.5),
            Anchor::Right => (1.0, 0.5),
            Anchor::BottomLeft => (0.0, 0.0),
            Anchor::Bottom => (0.5, 0.0),
            Anchor::BottomRight => (1.0, 0.0),
        }
    }
}

/// How big a resized object becomes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Size {
    /// Scale factors across and up the displayed page.
    Scale { sx: f64, sy: f64 },
    /// A displayed width and/or height in points; with only one, the other scales in proportion.
    To { width: Option<f64>, height: Option<f64> },
}

/// Scale factors accepted by Resize.
const MIN_SCALE: f64 = 1e-3;
const MAX_SCALE: f64 = 1e3;
/// The longest dash pattern accepted, and the largest width or dash length (points).
const MAX_DASH: usize = 16;
const MAX_LENGTH: f64 = 10_000.0;
/// The largest rotation accepted (degrees, either way).
const MAX_DEGREES: f64 = 1e6;

/// What to do with the selected objects.
#[derive(Clone, Debug, PartialEq)]
pub enum ObjectsEdit {
    Arrange(Arrange),
    /// Align to the selection's box, or to the page (its crop box) when one object is selected.
    Align(AlignTo),
    /// Equal gaps between three or more objects; the outermost two stay where they are.
    Distribute(Axis),
    /// Quarter turns clockwise, each object about its own centre.
    Rotate(i32),
    /// Turn each object about its own centre by this many degrees, clockwise as displayed (the
    /// Edit object tool's rotation handle).
    RotateBy(f64),
    /// Apply this user-space transform to every selected object (the Edit object tool scaling or
    /// turning several objects about their combined box).
    Matrix([f64; 6]),
    /// Mirror each object about its own centre: left to right (`horizontal`) or top to bottom.
    Flip {
        horizontal: bool,
    },
    /// Move by this offset in display space (points, right and up).
    Move {
        dx: f64,
        dy: f64,
    },
    /// Scale each object about an anchor of its own displayed box.
    Resize {
        size: Size,
        anchor: Anchor,
    },
    /// Change the appearance of the selected vector paths.
    Style(PathStyle),
    /// Remove the objects from the page.
    Delete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnitKind {
    Text,
    Path,
    Image,
    Form,
    InlineImage,
    Shading,
    /// `MP` / `DP`: kept in place, never listed.
    Point,
    /// A marked-content sequence with nothing painted inside: its `BMC`/`BDC` and `EMC`.
    EmptyMarked,
}

impl UnitKind {
    fn paints(self) -> bool {
        !matches!(self, UnitKind::Point | UnitKind::EmptyMarked)
    }
}

/// One painting operation (or text object) of the page's content.
#[derive(Clone, Debug)]
struct Unit {
    kind: UnitKind,
    /// Operator indices (the page's streams parsed joined), end exclusive.
    ops: Range<usize>,
    ctm: Matrix,
    /// The state operators that set up its graphics state from the content's start.
    prefix: Vec<Op>,
    /// State it leaves for later content (text state set inside it, a path's clip).
    residue: Vec<Op>,
    /// The enclosing `BMC`/`BDC` operators, outermost first.
    marked: Vec<usize>,
    rect: Option<[f64; 4]>,
    text: String,
    /// Why it can't be rewritten, if it can't.
    problem: Option<String>,
    /// For a path: the fill and stroke colour spaces it paints with.
    spaces: Option<(Object, Object)>,
    /// For a path: how it is painted.
    appearance: Option<Appearance>,
    /// The constant opacity it is painted with.
    opacity: f64,
}

#[derive(Clone, Debug)]
struct Group {
    units: Range<usize>,
    listed: bool,
    mcid: Option<i64>,
}

/// A page's content split into objects.
struct Scan {
    page: pdfcraft_model::Page,
    streams: Vec<(Object, Vec<u8>)>,
    pieces: Pieces,
    ops: Vec<Op>,
    /// The original content: these pieces of `/Contents`.
    region: Range<usize>,
    units: Vec<Unit>,
    groups: Vec<Group>,
    /// Indices of the listed groups.
    listed: Vec<usize>,
    /// The state the region leaves for what follows it: levels of state operators.
    end_state: Vec<Vec<Op>>,
    /// Why the page can't be rearranged, if it can't.
    problem: Option<String>,
}

#[derive(Clone)]
struct Gs {
    ctm: Matrix,
    line_width: f64,
    render_mode: f64,
    fill_cs: Object,
    stroke_cs: Object,
    /// The current fill and stroke colour components (empty: a pattern, or unknown).
    fill_color: Vec<f64>,
    stroke_color: Vec<f64>,
    /// Constant opacity (`/ca`, `/CA` of the `gs` in effect).
    fill_alpha: f64,
    stroke_alpha: f64,
    /// The box of the clipping paths in effect (device space), if any.
    clip: Option<[f64; 4]>,
}

impl Default for Gs {
    fn default() -> Self {
        Gs {
            ctm: Matrix::IDENTITY,
            line_width: 1.0,
            render_mode: 0.0,
            fill_cs: Object::name("DeviceGray"),
            stroke_cs: Object::name("DeviceGray"),
            fill_color: vec![0.0],
            stroke_color: vec![0.0],
            fill_alpha: 1.0,
            stroke_alpha: 1.0,
            clip: None,
        }
    }
}

/// The most colour components followed (a DeviceN space has at most 32).
const MAX_COMPONENTS: usize = 32;

/// The initial colour of a colour space selected with `cs` / `CS` (black, or unknown).
fn initial_color(space: &Object) -> Vec<f64> {
    match space.as_name() {
        Some(b"DeviceGray" | b"G") => vec![0.0],
        Some(b"DeviceRGB" | b"RGB") => vec![0.0; 3],
        Some(b"DeviceCMYK" | b"CMYK") => vec![0.0, 0.0, 0.0, 1.0],
        _ => Vec::new(),
    }
}

impl Gs {
    /// Follow the colour spaces and colours colour operators select.
    fn track_color(&mut self, op: &Op) {
        let nums = || op.operands.iter().take(MAX_COMPONENTS).filter_map(Object::as_f64).filter(|v| v.is_finite()).collect::<Vec<_>>();
        let (fill, space, color) = match op.op.as_slice() {
            b"g" => (true, Object::name("DeviceGray"), nums()),
            b"rg" => (true, Object::name("DeviceRGB"), nums()),
            b"k" => (true, Object::name("DeviceCMYK"), nums()),
            b"G" => (false, Object::name("DeviceGray"), nums()),
            b"RG" => (false, Object::name("DeviceRGB"), nums()),
            b"K" => (false, Object::name("DeviceCMYK"), nums()),
            b"cs" | b"CS" => match op.operands.first() {
                Some(o) => (op.is("cs"), o.clone(), initial_color(o)),
                None => return,
            },
            b"sc" | b"scn" => {
                self.fill_color = nums();
                return;
            }
            b"SC" | b"SCN" => {
                self.stroke_color = nums();
                return;
            }
            _ => return,
        };
        if fill {
            self.fill_cs = space;
            self.fill_color = color;
        } else {
            self.stroke_cs = space;
            self.stroke_color = color;
        }
    }

    /// Follow the constant opacity a `gs` operator sets (from the page's `/ExtGState`s).
    fn track_alpha(&mut self, doc: &Document, states: &Dict, op: &Op) {
        if !op.is("gs") {
            return;
        }
        let Some(state) = op.name(0).and_then(|n| states.get(n)).map(|s| doc.resolve(s)) else { return };
        let Some(state) = state.as_dict() else { return };
        let alpha = |key: &[u8]| state.get(key).and_then(Object::as_f64).filter(|v| v.is_finite()).map(|v| v.clamp(0.0, 1.0));
        if let Some(a) = alpha(b"ca") {
            self.fill_alpha = a;
        }
        if let Some(a) = alpha(b"CA") {
            self.stroke_alpha = a;
        }
    }

    /// How a path painted with operator `op` now looks.
    fn appearance(&self, op: &[u8]) -> Appearance {
        let fills = matches!(op, b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*");
        let [a, b, c, d, _, _] = self.ctm.0;
        let scale = (a * d - b * c).abs().sqrt();
        let width = self.line_width.abs() * scale;
        Appearance {
            fill: if fills { paint_of(&self.fill_cs, &self.fill_color) } else { Paint::None },
            stroke: if strokes(op) { paint_of(&self.stroke_cs, &self.stroke_color) } else { Paint::None },
            line_width: if width.is_finite() { width } else { 0.0 },
            fill_opacity: self.fill_alpha,
            stroke_opacity: self.stroke_alpha,
        }
    }
}

/// A colour as [`Paint`]: device colours by their space, anything else in its own space.
fn paint_of(space: &Object, color: &[f64]) -> Paint {
    match (space.as_name(), color) {
        (Some(b"DeviceGray" | b"G"), [g]) => Paint::Gray(*g),
        (Some(b"DeviceRGB" | b"RGB"), [r, g, b]) => Paint::Rgb([*r, *g, *b]),
        (Some(b"DeviceCMYK" | b"CMYK"), [c, m, y, k]) => Paint::Cmyk([*c, *m, *y, *k]),
        _ => Paint::Current(color.to_vec()),
    }
}

/// How a state operator combines with earlier ones at the same `q` level.
enum Class {
    /// A later one replaces an earlier one of the same keyword.
    Single,
    /// Sets the fill (or stroke) colour space and colour: replaces every earlier fill colour op.
    ColorAll(bool),
    /// Sets the fill (or stroke) colour in the current space.
    ColorValue(bool),
    /// Kept in order with everything else (`cm`, `gs`, clips, unknown operators).
    Ordered,
}

fn state_class(op: &[u8]) -> Option<Class> {
    Some(match op {
        b"w" | b"J" | b"j" | b"M" | b"d" | b"ri" | b"i" | b"Tc" | b"Tw" | b"Tz" | b"TL" | b"Ts" | b"Tr" | b"Tf" => Class::Single,
        b"g" | b"rg" | b"k" | b"cs" => Class::ColorAll(true),
        b"sc" | b"scn" => Class::ColorValue(true),
        b"G" | b"RG" | b"K" | b"CS" => Class::ColorAll(false),
        b"SC" | b"SCN" => Class::ColorValue(false),
        b"cm" | b"gs" => Class::Ordered,
        _ => return None,
    })
}

fn is_fill_color(op: &[u8]) -> bool {
    matches!(op, b"g" | b"rg" | b"k" | b"cs" | b"sc" | b"scn")
}

fn is_stroke_color(op: &[u8]) -> bool {
    matches!(op, b"G" | b"RG" | b"K" | b"CS" | b"SC" | b"SCN")
}

/// Add a state operator to a level, dropping earlier ones it makes irrelevant.
fn push_state(level: &mut Vec<Op>, op: Op) {
    match state_class(&op.op) {
        Some(Class::Single) => level.retain(|o| o.op != op.op),
        Some(Class::ColorAll(fill)) => level.retain(|o| if fill { !is_fill_color(&o.op) } else { !is_stroke_color(&o.op) }),
        Some(Class::ColorValue(fill)) => {
            let value: &[&[u8]] = if fill { &[b"sc", b"scn"] } else { &[b"SC", b"SCN"] };
            level.retain(|o| !value.contains(&o.op.as_slice()));
        }
        Some(Class::Ordered) | None => {}
    }
    level.push(op);
}

fn is_path_construction(op: &[u8]) -> bool {
    matches!(op, b"m" | b"l" | b"c" | b"v" | b"y" | b"h" | b"re")
}

fn is_path_painting(op: &[u8]) -> bool {
    matches!(op, b"S" | b"s" | b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" | b"n")
}

fn strokes(op: &[u8]) -> bool {
    matches!(op, b"S" | b"s" | b"B" | b"B*" | b"b" | b"b*")
}

fn union(a: Option<[f64; 4]>, b: Option<[f64; 4]>) -> Option<[f64; 4]> {
    match (a, b) {
        (Some(a), Some(b)) => Some([a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]),
        (a, None) => a,
        (None, b) => b,
    }
}

fn finite_rect(r: [f64; 4]) -> Option<[f64; 4]> {
    (r.iter().all(|v| v.is_finite()) && r[2] >= r[0] && r[3] >= r[1]).then_some(r)
}

fn page_of(doc: &Document, page: usize) -> Result<pdfcraft_model::Page, EditError> {
    pdfcraft_model::pages(doc).into_iter().nth(page).ok_or(EditError::NoSuchPage(page))
}

fn resources(doc: &Document, page: &Dict) -> Dict {
    page.get(b"Resources").map(|r| doc.resolve(r)).and_then(|r| r.as_dict().cloned()).unwrap_or_default()
}

fn sub_dict(doc: &Document, res: &Dict, key: &[u8]) -> Dict {
    res.get(key).map(|x| doc.resolve(x)).and_then(|x| x.as_dict().cloned()).unwrap_or_default()
}

/// The MCID a `BDC` operator's property list carries (inline or a `/Properties` resource).
fn mcid_of(doc: &Document, properties: &Dict, op: &Op) -> Option<i64> {
    if !op.is("BDC") {
        return None;
    }
    match op.operands.get(1)? {
        Object::Dict(d) => d.int(b"MCID"),
        Object::Name(n) => doc.resolve(properties.get(n)?).as_dict()?.int(b"MCID"),
        _ => None,
    }
}

/// The XObject a `Do` paints: (kind, its box in its own space and the matrix to the CTM's space).
fn xobject_box(doc: &Document, xobjects: &Dict, op: &Op) -> (UnitKind, Option<([f64; 4], Matrix)>) {
    let Some(obj) = op.name(0).and_then(|n| xobjects.get(n)).map(|r| doc.resolve(r)) else { return (UnitKind::Form, None) };
    let Object::Stream(s) = &*obj else { return (UnitKind::Form, None) };
    if s.dict.name(b"Subtype") == Some(b"Image") {
        return (UnitKind::Image, Some(([0.0, 0.0, 1.0, 1.0], Matrix::IDENTITY)));
    }
    let bbox = s.dict.get(b"BBox").map(|v| doc.resolve(v)).and_then(|v| {
        let a = v.as_array()?;
        let nums: Vec<f64> = a.iter().filter_map(Object::as_f64).collect();
        let [x0, y0, x1, y1] = <[f64; 4]>::try_from(nums).ok()?;
        Some([x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)])
    });
    let matrix = match s.dict.get(b"Matrix") {
        Some(v) => doc.resolve(v).as_array().and_then(|a| Matrix::from_operands(a)),
        None => Some(Matrix::IDENTITY),
    };
    (UnitKind::Form, bbox.zip(matrix))
}

/// An open path: where it starts, its box, its clip operator and state set inside it.
type OpenPath = (usize, Option<[f64; 4]>, Option<Op>, Vec<Op>);

/// Split page `page`'s original content into units and objects.
fn scan(doc: &Document, page: usize, with_prefix: bool) -> Result<Scan, EditError> {
    let p = page_of(doc, page)?;
    let streams = content_streams(doc, &p.dict);
    let pieces = Pieces::join(&streams.iter().map(|(_, d)| d.as_slice()).collect::<Vec<_>>());
    let ops = pieces.parse().ops;
    // The original content: the first run of streams PdfCraft didn't tag.
    let untagged: Vec<bool> = streams.iter().map(|(o, _)| crate::stream_tag(doc, o).is_none()).collect();
    let r0 = untagged.iter().position(|u| *u).unwrap_or(untagged.len());
    let r1 = untagged.iter().skip(r0).position(|u| !*u).map_or(untagged.len(), |k| r0.saturating_add(k));
    let region = r0..r1;
    let res = resources(doc, &p.dict);
    let (xobjects, properties) = (sub_dict(doc, &res, b"XObject"), sub_dict(doc, &res, b"Properties"));
    let states = sub_dict(doc, &res, b"ExtGState");
    let crop = p.crop(doc);

    let mut problem = None;
    // The CTM where the region starts (marks before it may wrap it in `q … cm`).
    let mut gs = Gs::default();
    let mut gstack: Vec<Gs> = Vec::new();
    let mut first = ops.len();
    for (i, op) in ops.iter().enumerate() {
        let (a, b) = pieces.pieces_of(op);
        if a >= region.start {
            if a < region.end && b >= region.end {
                problem = Some("an operator runs from the page content into a stream PdfCraft added".to_string());
            }
            first = i;
            break;
        }
        if b >= region.start {
            problem = Some("an operator runs from a stream PdfCraft added into the page content".to_string());
        }
        match op.op.as_slice() {
            b"q" => gstack.push(gs.clone()),
            b"Q" => gs = gstack.pop().unwrap_or(gs),
            b"cm" => {
                if let Some(m) = op.nums::<6>() {
                    gs.ctm = Matrix(m).then(&gs.ctm);
                }
            }
            _ => {}
        }
    }
    let last = ops.iter().enumerate().skip(first).find(|(_, op)| pieces.pieces_of(op).0 >= region.end).map_or(ops.len(), |(i, _)| i);

    let mut units: Vec<Unit> = Vec::new();
    let mut levels: Vec<Vec<Op>> = vec![Vec::new()];
    let mut gstack: Vec<Gs> = Vec::new();
    let mut marked: Vec<(usize, bool)> = Vec::new();
    let mut mcids: HashMap<usize, i64> = HashMap::new();
    let mut replayed = 0usize;
    // An open path: where it starts, its box, the clip operator and state set inside it.
    let mut path: Option<OpenPath> = None;
    // An open text object: where it starts, the state set inside it, marked-content depth inside
    // it, the CTM, the highest render mode, and its problem.
    struct OpenText {
        start: usize,
        residue: Vec<Op>,
        depth: i64,
        render_mode: f64,
        problem: Option<String>,
        ctm: Matrix,
    }
    let mut text: Option<OpenText> = None;

    let push_unit = |units: &mut Vec<Unit>,
                     marked: &mut Vec<(usize, bool)>,
                     levels: &[Vec<Op>],
                     replayed: &mut usize,
                     problem: &mut Option<String>,
                     mut unit: Unit| {
        for m in marked.iter_mut() {
            m.1 = true;
        }
        unit.marked = marked.iter().map(|m| m.0).collect();
        if with_prefix && unit.kind.paints() {
            let n: usize = levels.iter().map(Vec::len).sum();
            *replayed = replayed.saturating_add(n);
            if *replayed > MAX_REPLAYED {
                problem.get_or_insert_with(|| "the page is too complex to rearrange".to_string());
            } else {
                unit.prefix = levels.iter().flatten().cloned().collect();
            }
        }
        units.push(unit);
    };
    let unit = |kind: UnitKind, ops: Range<usize>, ctm: Matrix, rect: Option<[f64; 4]>| Unit {
        kind,
        ops,
        ctm,
        prefix: Vec::new(),
        residue: Vec::new(),
        marked: Vec::new(),
        rect: rect.and_then(finite_rect),
        text: String::new(),
        problem: None,
        spaces: None,
        appearance: None,
        opacity: 1.0,
    };

    for (i, op) in ops.iter().enumerate().take(last).skip(first) {
        let kw = op.op.as_slice();
        if pieces.pieces_of(op).1 >= region.end {
            problem.get_or_insert_with(|| "an operator runs from the page content into a stream PdfCraft added".to_string());
        }
        if let Some(t) = text.as_mut() {
            match kw {
                b"ET" => {
                    let Some(t) = text.take() else { continue };
                    let mut u = unit(UnitKind::Text, t.start..i.saturating_add(1), t.ctm, None);
                    u.opacity = gs.fill_alpha;
                    u.problem = t.problem.or_else(|| (t.depth != 0).then(|| "marked content crosses the edge of a text object".to_string()));
                    if t.render_mode >= 4.0 {
                        u.problem = Some("text used as a clipping path can't be rearranged or moved".to_string());
                    }
                    if let Some(level) = levels.last_mut() {
                        for r in &t.residue {
                            push_state(level, r.clone());
                        }
                    }
                    u.residue = t.residue;
                    push_unit(&mut units, &mut marked, &levels, &mut replayed, &mut problem, u);
                }
                b"BMC" | b"BDC" => t.depth = t.depth.saturating_add(1),
                b"EMC" => {
                    t.depth = t.depth.saturating_sub(1);
                    if t.depth < 0 {
                        t.problem = Some("marked content crosses the edge of a text object".to_string());
                    }
                }
                b"q" | b"Q" | b"cm" | b"BT" => t.problem = Some("the text object holds graphics state operators it may not".to_string()),
                b"\"" => {
                    if let (Some(aw), Some(ac)) = (op.num(0), op.num(1)) {
                        t.residue.push(Op::new("Tw", vec![pdfcraft_content::num(aw)]));
                        t.residue.push(Op::new("Tc", vec![pdfcraft_content::num(ac)]));
                    }
                }
                _ if state_class(kw).is_some() => {
                    if kw == b"Tr"
                        && let Some(v) = op.num(0)
                    {
                        gs.render_mode = v;
                        t.render_mode = t.render_mode.max(v);
                    }
                    gs.track_color(op);
                    gs.track_alpha(doc, &states, op);
                    if kw == b"w"
                        && let Some(v) = op.num(0)
                    {
                        gs.line_width = v;
                    }
                    t.residue.push(op.clone());
                }
                _ => {}
            }
            continue;
        }
        if let Some((_, rect, clip, inner)) = path.as_mut() {
            if is_path_construction(kw) {
                let pts: Vec<(f64, f64)> = if kw == b"re" {
                    op.nums::<4>().map(|[x, y, w, h]| vec![(x, y), (x + w, y), (x, y + h), (x + w, y + h)]).unwrap_or_default()
                } else {
                    op.operands.chunks(2).filter_map(|c| Some((c.first()?.as_f64()?, c.get(1)?.as_f64()?))).collect()
                };
                for (x, y) in pts {
                    let (x, y) = gs.ctm.apply(x, y);
                    *rect = union(*rect, Some([x, y, x, y]));
                }
                continue;
            }
            if matches!(kw, b"W" | b"W*") {
                *clip = Some(op.clone());
                continue;
            }
            if is_path_painting(kw) {
                let Some((start, rect, clip, inner)) = path.take() else { continue };
                let mut residue = inner;
                if clip.is_some()
                    && let Some(r) = rect
                {
                    gs.clip = Some(match gs.clip {
                        Some(c) => [c[0].max(r[0]), c[1].max(r[1]), c[2].min(r[2]), c[3].min(r[3])],
                        None => r,
                    });
                }
                if let Some(w) = &clip {
                    residue.extend(ops.get(start..i).unwrap_or_default().iter().filter(|o| is_path_construction(&o.op)).cloned());
                    residue.push(w.clone());
                    residue.push(Op::new("n", vec![]));
                }
                if kw != b"n" {
                    let grow = if strokes(kw) {
                        let [a, b, c, d, _, _] = gs.ctm.0;
                        gs.line_width.abs().max(0.0) * (a * d - b * c).abs().sqrt() / 2.0
                    } else {
                        0.0
                    };
                    let rect = rect.map(|r| [r[0] - grow, r[1] - grow, r[2] + grow, r[3] + grow]);
                    let mut u = unit(UnitKind::Path, start..i.saturating_add(1), gs.ctm, rect);
                    u.residue = residue.clone();
                    u.spaces = Some((gs.fill_cs.clone(), gs.stroke_cs.clone()));
                    u.opacity = if strokes(kw) && gs.appearance(kw).fill == Paint::None { gs.stroke_alpha } else { gs.fill_alpha };
                    u.appearance = Some(gs.appearance(kw));
                    // The unit's own state ops apply before it at this level for later units.
                    if let Some(level) = levels.last_mut() {
                        for r in residue {
                            if level.len() >= MAX_LEVEL {
                                problem.get_or_insert_with(|| "the page is too complex to rearrange".to_string());
                                break;
                            }
                            push_state(level, r);
                        }
                    }
                    push_unit(&mut units, &mut marked, &levels, &mut replayed, &mut problem, u);
                } else if let Some(level) = levels.last_mut() {
                    for r in residue {
                        push_state(level, r);
                    }
                }
                continue;
            }
            if state_class(kw).is_some() {
                // Not allowed inside a path, but kept: it applies after the path's object.
                inner.push(op.clone());
                gs.track_color(op);
                gs.track_alpha(doc, &states, op);
                if kw == b"w"
                    && let Some(v) = op.num(0)
                {
                    gs.line_width = v;
                }
                continue;
            }
            // Anything else ends the path without painting it (viewers drop such a path).
            path = None;
        }
        match kw {
            b"q" => {
                levels.push(Vec::new());
                gstack.push(gs.clone());
            }
            b"Q" => {
                if levels.len() > 1 {
                    levels.pop();
                    gs = gstack.pop().unwrap_or(gs);
                }
            }
            b"BT" => {
                text = Some(OpenText { start: i, residue: Vec::new(), depth: 0, render_mode: gs.render_mode, problem: None, ctm: gs.ctm });
            }
            _ if is_path_construction(kw) => {
                path = Some((i, None, None, Vec::new()));
                let pts: Vec<(f64, f64)> = if kw == b"re" {
                    op.nums::<4>().map(|[x, y, w, h]| vec![(x, y), (x + w, y), (x, y + h), (x + w, y + h)]).unwrap_or_default()
                } else {
                    op.operands.chunks(2).filter_map(|c| Some((c.first()?.as_f64()?, c.get(1)?.as_f64()?))).collect()
                };
                if let Some((_, rect, _, _)) = path.as_mut() {
                    for (x, y) in pts {
                        let (x, y) = gs.ctm.apply(x, y);
                        *rect = union(*rect, Some([x, y, x, y]));
                    }
                }
            }
            b"Do" => {
                let (kind, bbox) = xobject_box(doc, &xobjects, op);
                let rect = bbox.map(|(b, m)| m.then(&gs.ctm).bbox(b));
                let mut u = unit(kind, i..i.saturating_add(1), gs.ctm, rect);
                u.opacity = gs.fill_alpha;
                push_unit(&mut units, &mut marked, &levels, &mut replayed, &mut problem, u);
            }
            b"BI" => {
                let mut u = unit(UnitKind::InlineImage, i..i.saturating_add(1), gs.ctm, Some(gs.ctm.bbox([0.0, 0.0, 1.0, 1.0])));
                u.opacity = gs.fill_alpha;
                push_unit(&mut units, &mut marked, &levels, &mut replayed, &mut problem, u);
            }
            b"sh" => {
                // A shading fills the clip (and the page at most).
                let area = match gs.clip {
                    Some(c) => [c[0].max(crop[0]), c[1].max(crop[1]), c[2].min(crop[2]), c[3].min(crop[3])],
                    None => crop,
                };
                let mut u = unit(UnitKind::Shading, i..i.saturating_add(1), gs.ctm, Some(area));
                u.opacity = gs.fill_alpha;
                push_unit(&mut units, &mut marked, &levels, &mut replayed, &mut problem, u);
            }
            b"MP" | b"DP" => {
                let u = unit(UnitKind::Point, i..i.saturating_add(1), gs.ctm, None);
                push_unit(&mut units, &mut marked, &levels, &mut replayed, &mut problem, u);
            }
            b"BMC" | b"BDC" => {
                if let Some(id) = mcid_of(doc, &properties, op) {
                    mcids.insert(i, id);
                }
                marked.push((i, false));
            }
            b"EMC" => {
                if let Some((open, painted)) = marked.pop()
                    && !painted
                {
                    let u = unit(UnitKind::EmptyMarked, open..i.saturating_add(1), gs.ctm, None);
                    push_unit(&mut units, &mut marked, &levels, &mut replayed, &mut problem, u);
                }
            }
            b"BX" | b"EX" | b"d0" | b"d1" => {}
            _ if is_path_painting(kw) || matches!(kw, b"W" | b"W*") => {}
            _ => {
                if kw == b"cm"
                    && let Some(m) = op.nums::<6>()
                {
                    gs.ctm = Matrix(m).then(&gs.ctm);
                }
                if kw == b"w"
                    && let Some(v) = op.num(0)
                {
                    gs.line_width = v;
                }
                if kw == b"Tr"
                    && let Some(v) = op.num(0)
                {
                    gs.render_mode = v;
                }
                gs.track_color(op);
                gs.track_alpha(doc, &states, op);
                if let Some(level) = levels.last_mut() {
                    if level.len() >= MAX_LEVEL {
                        problem.get_or_insert_with(|| "the page is too complex to rearrange".to_string());
                    } else {
                        push_state(level, op.clone());
                    }
                }
            }
        }
    }
    if let Some(t) = text.take() {
        // A text object the content never ends: it runs to the end.
        let mut u = unit(UnitKind::Text, t.start..last, t.ctm, None);
        u.problem = Some("a text object is never ended".to_string());
        push_unit(&mut units, &mut marked, &levels, &mut replayed, &mut problem, u);
    }

    // Text boxes and what they show, from the text lines whose operators each object holds.
    if let Ok(lines) = crate::text::text_lines(doc, page) {
        for line in lines {
            let Some(&at) = line.source_ops().first() else { continue };
            let k = units.partition_point(|u| u.ops.end <= at);
            if let Some(u) = units.get_mut(k).filter(|u| u.ops.contains(&at) && u.kind == UnitKind::Text) {
                u.rect = union(u.rect, finite_rect(line.rect));
                if !u.text.is_empty() {
                    u.text.push(' ');
                }
                u.text.push_str(&line.text);
            }
        }
    }

    // Objects: units, joined while they sit in the same marked-content sequence with an MCID.
    let key = |u: &Unit| u.marked.iter().find_map(|m| mcids.get(m).map(|id| (*m, *id)));
    let mut groups: Vec<Group> = Vec::new();
    let mut last_key = None;
    for (k, u) in units.iter().enumerate() {
        let this = key(u);
        match groups.last_mut() {
            Some(g) if this.is_some() && this.map(|t| t.0) == last_key => {
                g.units.end = k.saturating_add(1);
                g.listed |= u.kind.paints();
            }
            _ => groups.push(Group { units: k..k.saturating_add(1), listed: u.kind.paints(), mcid: this.map(|t| t.1) }),
        }
        last_key = this.map(|t| t.0);
    }
    let listed = groups.iter().enumerate().filter(|(_, g)| g.listed).map(|(i, _)| i).collect();
    Ok(Scan { page: p, streams, pieces, ops, region, units, groups, listed, end_state: levels, problem })
}

impl Scan {
    fn object(&self, g: &Group) -> PageObject {
        let units = self.units.get(g.units.clone()).unwrap_or_default();
        let mut kinds = units.iter().filter(|u| u.kind.paints()).map(|u| u.kind);
        let first = kinds.next();
        let kind = match first {
            Some(k) if kinds.all(|o| o == k) => match k {
                UnitKind::Text => ObjectKind::Text,
                UnitKind::Path => ObjectKind::Path,
                UnitKind::Image => ObjectKind::Image,
                UnitKind::Form => ObjectKind::Form,
                UnitKind::InlineImage => ObjectKind::InlineImage,
                _ => ObjectKind::Shading,
            },
            _ => ObjectKind::Group,
        };
        let rect = units.iter().filter(|u| u.kind.paints()).fold(None, |r, u| union(r, u.rect));
        let text = units.iter().filter(|u| !u.text.is_empty()).map(|u| u.text.as_str()).collect::<Vec<_>>().join(" ");
        let appearance = units.iter().find_map(|u| u.appearance.clone());
        let opacity = units.iter().find(|u| u.kind.paints()).map_or(1.0, |u| u.opacity);
        PageObject { kind, rect, text, mcid: g.mcid, appearance, opacity }
    }

    /// The groups of the selected objects (indexes into the listed objects), deduplicated.
    fn selected(&self, objects: &[usize]) -> Result<Vec<usize>, EditError> {
        if objects.is_empty() {
            return Err(EditError::Invalid("select at least one object".into()));
        }
        let mut out = Vec::new();
        let mut seen = vec![false; self.groups.len()];
        for &o in objects {
            let g = *self.listed.get(o).ok_or_else(|| {
                EditError::Invalid(format!("object {} is out of range: the page has {} objects", o.saturating_add(1), self.listed.len()))
            })?;
            if let Some(seen) = seen.get_mut(g).filter(|s| !**s) {
                *seen = true;
                out.push(g);
            }
        }
        Ok(out)
    }

    fn group_rect(&self, g: usize) -> Option<[f64; 4]> {
        let g = self.groups.get(g)?;
        self.units.get(g.units.clone())?.iter().filter(|u| u.kind.paints()).fold(None, |r, u| union(r, u.rect))
    }

    fn op_bytes(&self, op: &Op, out: &mut Vec<u8>) {
        match self.pieces.data().get(op.span.clone()).filter(|b| !b.is_empty()) {
            Some(bytes) => {
                out.extend_from_slice(bytes);
                out.push(b'\n');
            }
            None => write_op(op, out),
        }
    }
}

/// The objects page `page` (0-based) draws in its own content, in paint order (back to front).
pub fn page_objects(doc: &Document, page: usize) -> Result<Vec<PageObject>, EditError> {
    let s = scan(doc, page, false)?;
    Ok(s.listed.iter().filter_map(|g| s.groups.get(*g)).map(|g| s.object(g)).collect())
}

/// Cells per side of an [`ObjectIndex`]'s grid.
const GRID: usize = 32;
/// The most grid cells one object is entered in; larger objects are kept on a short list that
/// every query checks.
const MAX_CELLS: usize = 16;

/// A hit-testing index of a page's objects (the Edit object tool's clicks and marquees): built
/// once from [`page_objects`] when the page changes, then each query looks only at the objects
/// in the grid cells it touches. Objects without a box can't be hit.
#[derive(Clone, Debug, Default)]
pub struct ObjectIndex {
    rects: Vec<Option<[f64; 4]>>,
    /// The grid's extent in user space, and the size of a cell.
    origin: [f64; 2],
    cell: [f64; 2],
    cells: Vec<Vec<u32>>,
    /// Objects too large for the grid (or every object, when the extent can't be gridded).
    large: Vec<u32>,
}

impl ObjectIndex {
    pub fn new(objects: &[PageObject]) -> ObjectIndex {
        let rects: Vec<Option<[f64; 4]>> = objects.iter().map(|o| o.rect.and_then(finite_rect)).collect();
        let bounds = rects.iter().fold(None, |b, r| union(b, *r));
        let mut index = ObjectIndex { rects, ..ObjectIndex::default() };
        let Some(b) = bounds else { return index };
        let (w, h) = (b[2] - b[0], b[3] - b[1]);
        let griddable = w.is_finite() && h.is_finite() && w > 1e-9 && h > 1e-9 && index.rects.len() <= u32::MAX as usize;
        if !griddable {
            index.large = (0..index.rects.len()).filter(|k| index.rects.get(*k).is_some_and(Option::is_some)).map(|k| k as u32).collect();
            return index;
        }
        index.origin = [b[0], b[1]];
        index.cell = [w / GRID as f64, h / GRID as f64];
        index.cells = vec![Vec::new(); GRID * GRID];
        for (k, r) in index.rects.iter().enumerate() {
            let Some(r) = r else { continue };
            let (cols, rows) = index.span(*r);
            if cols.len().saturating_mul(rows.len()) > MAX_CELLS {
                index.large.push(k as u32);
                continue;
            }
            for row in rows {
                for col in cols.clone() {
                    if let Some(c) = index.cells.get_mut(row * GRID + col) {
                        c.push(k as u32);
                    }
                }
            }
        }
        index
    }

    /// How many objects it holds.
    pub fn len(&self) -> usize {
        self.rects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }

    /// The columns and rows of cells `r` touches.
    fn span(&self, r: [f64; 4]) -> (Range<usize>, Range<usize>) {
        let at = |v: f64, o: f64, size: f64| -> usize {
            let k = ((v - o) / size).floor();
            if k.is_nan() || k < 0.0 { 0 } else { (k as usize).min(GRID - 1) }
        };
        let (c0, c1) = (at(r[0], self.origin[0], self.cell[0]), at(r[2], self.origin[0], self.cell[0]));
        let (r0, r1) = (at(r[1], self.origin[1], self.cell[1]), at(r[3], self.origin[1], self.cell[1]));
        (c0..c1.saturating_add(1), r0..r1.saturating_add(1))
    }

    /// The objects whose boxes may touch `r`, each once, in paint order.
    fn candidates(&self, r: [f64; 4]) -> Vec<usize> {
        let mut out: Vec<usize> = self.large.iter().map(|k| *k as usize).collect();
        if !self.cells.is_empty() {
            let (cols, rows) = self.span(r);
            for row in rows {
                for col in cols.clone() {
                    out.extend(self.cells.get(row * GRID + col).into_iter().flatten().map(|k| *k as usize));
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The objects under user-space point (`x`, `y`), within `slop` points of their boxes,
    /// topmost first.
    pub fn hits(&self, x: f64, y: f64, slop: f64) -> Vec<usize> {
        if !(x.is_finite() && y.is_finite()) {
            return Vec::new();
        }
        let slop = if slop.is_finite() { slop.clamp(0.0, 1e4) } else { 0.0 };
        let probe = [x - slop, y - slop, x + slop, y + slop];
        let mut out: Vec<usize> = self
            .candidates(probe)
            .into_iter()
            .filter(|k| {
                self.rects.get(*k).copied().flatten().is_some_and(|r| r[0] <= probe[2] && probe[0] <= r[2] && r[1] <= probe[3] && probe[1] <= r[3])
            })
            .collect();
        out.reverse();
        out
    }

    /// The topmost object under user-space point (`x`, `y`) (within `slop` points).
    pub fn hit(&self, x: f64, y: f64, slop: f64) -> Option<usize> {
        self.hits(x, y, slop).into_iter().next()
    }

    /// The objects whose boxes touch user-space rectangle `r` (a marquee), in paint order.
    pub fn touching(&self, r: [f64; 4]) -> Vec<usize> {
        if !r.iter().all(|v| v.is_finite()) {
            return Vec::new();
        }
        let r = [r[0].min(r[2]), r[1].min(r[3]), r[0].max(r[2]), r[1].max(r[3])];
        self.candidates(r)
            .into_iter()
            .filter(|k| self.rects.get(*k).copied().flatten().is_some_and(|b| b[0] <= r[2] && r[0] <= b[2] && b[1] <= r[3] && r[1] <= b[3]))
            .collect()
    }
}

/// User space → display space (after `/Rotate`), linear part only.
fn display(page: &pdfcraft_model::Page) -> Matrix {
    match page.dict.int(b"Rotate").unwrap_or(0).rem_euclid(360) / 90 {
        1 => Matrix([0.0, -1.0, 1.0, 0.0, 0.0, 0.0]),
        2 => Matrix([-1.0, 0.0, 0.0, -1.0, 0.0, 0.0]),
        3 => Matrix([0.0, 1.0, -1.0, 0.0, 0.0, 0.0]),
        _ => Matrix::IDENTITY,
    }
}

/// What an edit does: a new paint order of the groups, a user-space transform per group, new
/// state for path units, or units to remove.
enum Plan {
    Order(Vec<usize>),
    Transform(Vec<(usize, [f64; 6])>),
    Style(Styled),
    Delete(Vec<usize>),
}

/// Restyled path units: (unit index, state operators to set first, the new painting operator),
/// and the opacity ExtGState to add to the page's resources (its name and value).
struct Styled {
    /// `None` for the painting operator: the unit is kept whole (opacity only).
    units: Vec<(usize, Vec<u8>, Option<&'static str>)>,
    opacity: Option<(Vec<u8>, f64)>,
}

fn is_identity(t: &[f64; 6]) -> bool {
    let id = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    t.iter().zip(id).all(|(a, b)| (a - b).abs() < 1e-9)
}

/// Move runs of selected groups up (`up`) past the next unselected object that overlaps them.
fn step(s: &Scan, order: &mut Vec<usize>, mask: &[bool], up: bool) {
    let is_sel = |g: &usize| mask.get(*g).copied().unwrap_or(false);
    let listed = |g: &usize| s.groups.get(*g).is_some_and(|g| g.listed);
    // Runs of consecutive selected groups: (start, end exclusive) positions.
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for (pos, g) in order.iter().enumerate() {
        if is_sel(g) {
            match runs.last_mut() {
                Some(r) if r.1 == pos => r.1 = pos.saturating_add(1),
                _ => runs.push((pos, pos.saturating_add(1))),
            }
        }
    }
    if up {
        runs.reverse();
    }
    for (start, end) in runs {
        // Positions are still valid: runs on the moving side moved away from this one only.
        let Some(run) = order.get(start..end).map(<[usize]>::to_vec) else { continue };
        let rect = run.iter().fold(None, |r, g| union(r, s.group_rect(*g)));
        let overlapping = |g: &usize| match (rect, s.group_rect(*g)) {
            (Some(a), Some(b)) => pdfcraft_content::overlaps(a, b, 0.0),
            _ => true,
        };
        // Candidates: up to the next selected group in that direction.
        let range: Vec<usize> = if up {
            (end..order.len()).take_while(|p| order.get(*p).is_some_and(|g| !is_sel(g))).collect()
        } else {
            (0..start).rev().take_while(|p| order.get(*p).is_some_and(|g| !is_sel(g))).collect()
        };
        let target = range
            .iter()
            .copied()
            .find(|p| order.get(*p).is_some_and(|g| listed(g) && overlapping(g)))
            .or_else(|| range.iter().copied().find(|p| order.get(*p).is_some_and(listed)));
        let Some(target) = target else { continue };
        let moved: Vec<usize> = order.drain(start..end).collect();
        let at = if up { target.saturating_sub(moved.len()).saturating_add(1) } else { target };
        let at = at.min(order.len());
        order.splice(at..at, moved);
    }
}

fn plan(doc: &Document, s: &Scan, objects: &[usize], edit: &ObjectsEdit) -> Result<Plan, EditError> {
    let selected = s.selected(objects)?;
    let number = |g: usize| s.listed.iter().position(|l| *l == g).map_or(0, |n| n.saturating_add(1));
    let rect_of = |g: usize| -> Result<[f64; 4], EditError> {
        s.group_rect(g)
            .ok_or_else(|| EditError::Invalid(format!("object {} has no box that can be measured, so it can only be arranged or moved", number(g))))
    };
    let d = display(&s.page);
    let inv = d.invert().unwrap_or(Matrix::IDENTITY);
    // A display-space offset → a user-space translation.
    let shift = |dx: f64, dy: f64| -> [f64; 6] {
        let (x, y) = inv.apply(dx, dy);
        [1.0, 0.0, 0.0, 1.0, x, y]
    };
    let shown = |g: usize| -> Result<[f64; 4], EditError> { Ok(d.bbox(rect_of(g)?)) };
    let transforms: Vec<(usize, [f64; 6])> = match edit {
        ObjectsEdit::Arrange(a) => {
            let mut order: Vec<usize> = (0..s.groups.len()).collect();
            let mut mask = vec![false; s.groups.len()];
            for g in &selected {
                if let Some(m) = mask.get_mut(*g) {
                    *m = true;
                }
            }
            let is_sel = |g: &usize| mask.get(*g).copied().unwrap_or(false);
            match *a {
                Arrange::Front => {
                    let (mut rest, sel): (Vec<usize>, Vec<usize>) = order.iter().partition(|g| !is_sel(g));
                    rest.extend(sel);
                    order = rest;
                }
                Arrange::Back => {
                    let (mut sel, rest): (Vec<usize>, Vec<usize>) = order.iter().partition(|g| is_sel(g));
                    sel.extend(rest);
                    order = sel;
                }
                Arrange::Forward => step(s, &mut order, &mask, true),
                Arrange::Backward => step(s, &mut order, &mask, false),
            }
            return Ok(Plan::Order(order));
        }
        ObjectsEdit::Align(to) => {
            let rects: Vec<(usize, [f64; 4])> = selected.iter().map(|g| Ok((*g, shown(*g)?))).collect::<Result<_, EditError>>()?;
            let reference = match rects.as_slice() {
                [_] => d.bbox(s.page.crop(doc)),
                _ => rects.iter().fold(None, |r, (_, b)| union(r, Some(*b))).unwrap_or([0.0; 4]),
            };
            let mid = |a: f64, b: f64| (a + b) / 2.0;
            rects
                .iter()
                .map(|(g, r)| {
                    let (dx, dy) = match to {
                        AlignTo::Left => (reference[0] - r[0], 0.0),
                        AlignTo::Right => (reference[2] - r[2], 0.0),
                        AlignTo::HorizontalCenter => (mid(reference[0], reference[2]) - mid(r[0], r[2]), 0.0),
                        AlignTo::Bottom => (0.0, reference[1] - r[1]),
                        AlignTo::Top => (0.0, reference[3] - r[3]),
                        AlignTo::VerticalCenter => (0.0, mid(reference[1], reference[3]) - mid(r[1], r[3])),
                    };
                    (*g, shift(dx, dy))
                })
                .collect()
        }
        ObjectsEdit::Distribute(axis) => {
            if selected.len() < 3 {
                return Err(EditError::Invalid("distributing needs at least three objects".into()));
            }
            let (lo, hi) = match axis {
                Axis::Horizontal => (0, 2),
                Axis::Vertical => (1, 3),
            };
            let mut rects: Vec<(usize, f64, f64)> = selected
                .iter()
                .map(|g| {
                    let r = shown(*g)?;
                    Ok((*g, r.get(lo).copied().unwrap_or(0.0), r.get(hi).copied().unwrap_or(0.0)))
                })
                .collect::<Result<_, EditError>>()?;
            rects.sort_by(|a, b| (a.1 + a.2).total_cmp(&(b.1 + b.2)));
            let (Some(first), Some(last)) = (rects.first().copied(), rects.last().copied()) else {
                return Err(EditError::Invalid("distributing needs at least three objects".into()));
            };
            let middle = rects.get(1..rects.len().saturating_sub(1)).unwrap_or_default();
            let widths: f64 = middle.iter().map(|(_, a, b)| b - a).sum();
            let gaps = middle.len().saturating_add(1) as f64;
            let gap = (last.1 - first.2 - widths) / gaps;
            let mut at = first.2 + gap;
            let mut out = Vec::new();
            for (g, a, b) in middle {
                let delta = at - a;
                out.push((*g, if lo == 0 { shift(delta, 0.0) } else { shift(0.0, delta) }));
                at += (b - a) + gap;
            }
            out
        }
        ObjectsEdit::Rotate(quarters) => {
            selected.iter().map(|g| Ok((*g, crate::turn_about_centre(rect_of(*g)?, *quarters, false, false)))).collect::<Result<_, EditError>>()?
        }
        ObjectsEdit::RotateBy(degrees) => {
            if !(degrees.is_finite() && degrees.abs() <= MAX_DEGREES) {
                return Err(EditError::Invalid(format!("the angle must be a finite number of degrees, at most {MAX_DEGREES} either way")));
            }
            let (sin, cos) = degrees.rem_euclid(360.0).to_radians().sin_cos();
            // Exact quarter turns stay exact.
            let snap = |v: f64| if (v - v.round()).abs() < 1e-12 { v.round() } else { v };
            let (sin, cos) = (snap(sin), snap(cos));
            selected
                .iter()
                .map(|g| {
                    let r = rect_of(*g)?;
                    let (cx, cy) = ((r[0] + r[2]) / 2.0, (r[1] + r[3]) / 2.0);
                    // Clockwise with y up; about the centre.
                    let (a, b, c, d) = (cos, -sin, sin, cos);
                    Ok((*g, [a, b, c, d, cx - a * cx - c * cy, cy - b * cx - d * cy]))
                })
                .collect::<Result<_, EditError>>()?
        }
        ObjectsEdit::Matrix(m) => {
            let [a, b, c, d, e, f] = *m;
            if !m.iter().all(|v| v.is_finite()) || [a, b, c, d].iter().any(|v| v.abs() > MAX_SCALE) || e.abs() > 1e7 || f.abs() > 1e7 {
                return Err(EditError::Invalid(format!(
                    "the matrix must be finite, its scale parts at most {MAX_SCALE} and its offsets at most 10,000,000 points"
                )));
            }
            if (a * d - b * c).abs() < MIN_SCALE * MIN_SCALE {
                return Err(EditError::Invalid("the matrix would flatten the objects to nothing".into()));
            }
            selected.iter().map(|g| (*g, *m)).collect()
        }
        ObjectsEdit::Flip { horizontal } => {
            // Left–right as displayed is top–bottom in user space on a page turned a quarter.
            let turned = d.0.first().is_some_and(|a| a.abs() < 0.5);
            let user_horizontal = *horizontal != turned;
            selected
                .iter()
                .map(|g| Ok((*g, crate::turn_about_centre(rect_of(*g)?, 0, user_horizontal, !user_horizontal))))
                .collect::<Result<_, EditError>>()?
        }
        ObjectsEdit::Move { dx, dy } => {
            if !(dx.is_finite() && dy.is_finite()) || dx.abs() > 1e6 || dy.abs() > 1e6 {
                return Err(EditError::Invalid("the offset must be finite and at most 1,000,000 points".into()));
            }
            selected.iter().map(|g| (*g, shift(*dx, *dy))).collect()
        }
        ObjectsEdit::Resize { size, anchor } => selected
            .iter()
            .map(|g| {
                let r = shown(*g)?;
                let (sx, sy) = scale_factors(size, r[2] - r[0], r[3] - r[1], number(*g))?;
                let (fx, fy) = anchor.fractions();
                let (ax, ay) = (r[0] + (r[2] - r[0]) * fx, r[1] + (r[3] - r[1]) * fy);
                let scale = Matrix([sx, 0.0, 0.0, sy, ax - sx * ax, ay - sy * ay]);
                Ok((*g, d.then(&scale).then(&inv).0))
            })
            .collect::<Result<_, EditError>>()?,
        ObjectsEdit::Style(style) => return Ok(Plan::Style(style_plan(doc, s, &selected, style, number)?)),
        ObjectsEdit::Delete => {
            let units = selected
                .iter()
                .filter_map(|g| s.groups.get(*g))
                .flat_map(|g| g.units.clone())
                .filter(|u| s.units.get(*u).is_some_and(|u| u.kind.paints()))
                .collect::<Vec<_>>();
            if let Some(p) = units.iter().filter_map(|u| s.units.get(*u)).find_map(|u| u.problem.as_ref()) {
                return Err(EditError::Invalid(format!("this object can't be deleted: {p}")));
            }
            return Ok(Plan::Delete(units));
        }
    };
    Ok(Plan::Transform(transforms.into_iter().filter(|(_, t)| !is_identity(t)).collect()))
}

/// The display-space scale factors that resize a `w`×`h` box (object number `n`) as `size` asks.
fn scale_factors(size: &Size, w: f64, h: f64, n: usize) -> Result<(f64, f64), EditError> {
    let (sx, sy) = match *size {
        Size::Scale { sx, sy } => (sx, sy),
        Size::To { width, height } => {
            let factor = |target: Option<f64>, have: f64, what: &str| -> Result<Option<f64>, EditError> {
                let Some(t) = target else { return Ok(None) };
                if !(t.is_finite() && t > 0.0 && t <= 1e6) {
                    return Err(EditError::Invalid(format!("the {what} must be more than 0 and at most 1,000,000 points")));
                }
                if !(have.is_finite() && have > 1e-6) {
                    return Err(EditError::Invalid(format!("object {n} has no {what} to resize")));
                }
                Ok(Some(t / have))
            };
            match (factor(width, w, "width")?, factor(height, h, "height")?) {
                (Some(x), Some(y)) => (x, y),
                (Some(x), None) => (x, x),
                (None, Some(y)) => (y, y),
                (None, None) => return Err(EditError::Invalid("give a width, a height or scale factors".into())),
            }
        }
    };
    if ![sx, sy].iter().all(|v| v.is_finite() && (MIN_SCALE..=MAX_SCALE).contains(v)) {
        return Err(EditError::Invalid(format!(
            "object {n} would be scaled by {sx} × {sy}: scale factors must be between {MIN_SCALE} and {MAX_SCALE}"
        )));
    }
    Ok((sx, sy))
}

fn check_paint(paint: Option<&Paint>, what: &str) -> Result<(), EditError> {
    let unit = |v: &[f64]| v.iter().all(|c| c.is_finite() && (0.0..=1.0).contains(c));
    let ok = match paint {
        None | Some(Paint::None) => true,
        Some(Paint::Gray(g)) => unit(&[*g]),
        Some(Paint::Rgb(c)) => unit(c),
        Some(Paint::Cmyk(c)) => unit(c),
        Some(Paint::Current(c)) => {
            if c.is_empty() || c.len() > 32 || !c.iter().all(|v| v.is_finite()) {
                return Err(EditError::Invalid(format!("the {what} colour needs 1 to 32 finite components")));
            }
            true
        }
    };
    if ok { Ok(()) } else { Err(EditError::Invalid(format!("the {what} colour's components must be between 0 and 1"))) }
}

/// The painting operator a path gets when its fill and/or stroke are turned on or off; `None`
/// when it would paint nothing.
fn new_paint_op(op: &[u8], fill: Option<&Paint>, stroke: Option<&Paint>) -> Option<&'static str> {
    let close = matches!(op, b"s" | b"b" | b"b*");
    let even_odd = matches!(op, b"f*" | b"B*" | b"b*");
    let fills = match fill {
        Some(Paint::None) => false,
        Some(_) => true,
        None => matches!(op, b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*"),
    };
    let stroked = match stroke {
        Some(Paint::None) => false,
        Some(_) => true,
        None => strokes(op),
    };
    Some(match (fills, stroked, close, even_odd) {
        (true, true, true, false) => "b",
        (true, true, true, true) => "b*",
        (true, true, false, false) => "B",
        (true, true, false, true) => "B*",
        (true, false, _, false) => "f",
        (true, false, _, true) => "f*",
        (false, true, true, _) => "s",
        (false, true, false, _) => "S",
        (false, false, _, _) => return None,
    })
}

/// The operators that set a fill (`fill`) or stroke colour, appended to `out`.
fn paint_ops(doc: &Document, res: &Dict, paint: &Paint, space: Option<&Object>, fill: bool, n: usize, out: &mut String) -> Result<(), EditError> {
    let what = if fill { "fill" } else { "stroke" };
    let join = |c: &[f64]| c.iter().map(|v| number(*v)).collect::<Vec<_>>().join(" ");
    let op = |o: &str| if fill { o.to_string() } else { o.to_uppercase() };
    let line = match paint {
        Paint::None => return Ok(()),
        Paint::Gray(g) => format!("{} {}", number(*g), op("g")),
        Paint::Rgb(c) => format!("{} {}", join(c), op("rg")),
        Paint::Cmyk(c) => format!("{} {}", join(c), op("k")),
        Paint::Current(c) => {
            let ranges = space.and_then(|cs| crate::convert_colors::color_ranges(doc, cs, res)).ok_or_else(|| {
                EditError::Invalid(format!(
                    "object {n} paints its {what} with a pattern or a colour space PdfCraft can't read: give a gray, RGB or CMYK colour"
                ))
            })?;
            if ranges.len() != c.len() {
                return Err(EditError::Invalid(format!("object {n}'s {what} colour space has {} components, not {}", ranges.len(), c.len())));
            }
            if let Some((v, r)) = c.iter().zip(&ranges).find(|(v, r)| !(r[0] - 1e-9..=r[1] + 1e-9).contains(*v)) {
                return Err(EditError::Invalid(format!("object {n}'s {what} component {v} is outside its range {} to {}", r[0], r[1])));
            }
            format!("{} {}", join(c), op("scn"))
        }
    };
    out.push_str(&line);
    out.push(' ');
    Ok(())
}

/// A name for a new opacity ExtGState that the page's resources don't use yet.
fn opacity_name(doc: &Document, res: &Dict, opacity: f64) -> Vec<u8> {
    let states = sub_dict(doc, res, b"ExtGState");
    let base = format!("PCOpacity{}", (opacity * 1000.0).round() as i64);
    let mut name = base.clone();
    let mut k = 1u32;
    while states.contains(name.as_bytes()) && k < 100_000 {
        name = format!("{base}_{k}");
        k = k.saturating_add(1);
    }
    name.into_bytes()
}

/// Check a style and work out what it writes for each selected path.
fn style_plan(doc: &Document, s: &Scan, selected: &[usize], style: &PathStyle, number_of: impl Fn(usize) -> usize) -> Result<Styled, EditError> {
    if *style == PathStyle::default() {
        return Err(EditError::Invalid("give at least one of fill, stroke, line width, dash, cap, join or opacity".into()));
    }
    check_paint(style.fill.as_ref(), "fill")?;
    check_paint(style.stroke.as_ref(), "stroke")?;
    let length = |v: &f64| v.is_finite() && (0.0..=MAX_LENGTH).contains(v);
    if style.line_width.is_some_and(|w| !length(&w)) {
        return Err(EditError::Invalid(format!("the line width must be between 0 and {MAX_LENGTH} points")));
    }
    if let Some(d) = &style.dash {
        if d.pattern.len() > MAX_DASH || !d.pattern.iter().chain([&d.phase]).all(length) {
            return Err(EditError::Invalid(format!(
                "a dash pattern has at most {MAX_DASH} lengths, each (and the phase) between 0 and {MAX_LENGTH} points"
            )));
        }
        if !d.pattern.is_empty() && d.pattern.iter().all(|v| *v == 0.0) {
            return Err(EditError::Invalid("a dash pattern's lengths can't all be zero".into()));
        }
    }
    if style.opacity.is_some_and(|o| !(o.is_finite() && (0.0..=1.0).contains(&o))) {
        return Err(EditError::Invalid("the opacity must be between 0 and 1".into()));
    }
    let res = resources(doc, &s.page.dict);
    let opacity = style.opacity.map(|o| (opacity_name(doc, &res, o), o));
    let mut units = Vec::new();
    // Opacity alone applies to any object (text, images, forms, shadings) as a whole.
    let opacity_only = *style == PathStyle { opacity: style.opacity, ..PathStyle::default() };
    for g in selected {
        let Some(group) = s.groups.get(*g) else { continue };
        let n = number_of(*g);
        let paths: Vec<usize> = group.units.clone().filter(|u| s.units.get(*u).is_some_and(|u| u.kind == UnitKind::Path)).collect();
        if opacity_only {
            for k in group.units.clone().filter(|u| s.units.get(*u).is_some_and(|u| u.kind.paints() && u.kind != UnitKind::Path)) {
                if let Some(p) = s.units.get(k).and_then(|u| u.problem.as_ref()) {
                    return Err(EditError::Invalid(format!("object {n} can't be changed: {p}")));
                }
                if let Some((name, _)) = &opacity {
                    let mut state = b"/".to_vec();
                    state.extend_from_slice(name);
                    state.extend_from_slice(b" gs ");
                    units.push((k, state, None));
                }
            }
        } else if paths.is_empty() {
            return Err(EditError::Invalid(format!("object {n} is not a vector path: only its opacity can change")));
        }
        for k in paths {
            let Some(u) = s.units.get(k) else { continue };
            if let Some(p) = &u.problem {
                return Err(EditError::Invalid(format!("object {n} can't be changed: {p}")));
            }
            let Some(paint) = u.ops.end.checked_sub(1).and_then(|e| s.ops.get(e)) else { continue };
            let op = new_paint_op(&paint.op, style.fill.as_ref(), style.stroke.as_ref())
                .ok_or_else(|| EditError::Invalid(format!("object {n} would paint nothing without fill and stroke: delete it instead")))?;
            let mut out = String::new();
            if let Some((name, _)) = &opacity {
                out.push('/');
                out.push_str(&String::from_utf8_lossy(name));
                out.push_str(" gs ");
            }
            if style.line_width.is_some() || style.dash.is_some() {
                // Displayed points → the path's user space.
                let [a, b, c, d, _, _] = u.ctm.0;
                let scale = (a * d - b * c).abs().sqrt();
                if !(scale.is_finite() && scale > 1e-9) {
                    return Err(EditError::Invalid(format!("object {n} is drawn with no area")));
                }
                if let Some(w) = style.line_width {
                    out.push_str(&format!("{} w ", number(w / scale)));
                }
                if let Some(dash) = &style.dash {
                    let pattern = dash.pattern.iter().map(|v| number(v / scale)).collect::<Vec<_>>().join(" ");
                    out.push_str(&format!("[{pattern}] {} d ", number(dash.phase / scale)));
                }
            }
            if let Some(cap) = style.cap {
                out.push_str(match cap {
                    LineCap::Butt => "0 J ",
                    LineCap::Round => "1 J ",
                    LineCap::Square => "2 J ",
                });
            }
            if let Some(join) = style.join {
                out.push_str(match join {
                    LineJoin::Miter => "0 j ",
                    LineJoin::Round => "1 j ",
                    LineJoin::Bevel => "2 j ",
                });
            }
            let spaces = u.spaces.as_ref();
            if let Some(p) = &style.fill {
                paint_ops(doc, &res, p, spaces.map(|s| &s.0), true, n, &mut out)?;
            }
            if let Some(p) = &style.stroke {
                paint_ops(doc, &res, p, spaces.map(|s| &s.1), false, n, &mut out)?;
            }
            units.push((k, out.into_bytes(), Some(op)));
        }
    }
    Ok(Styled { units, opacity })
}

/// Whether [`edit_objects`] would change the page (it refuses what it can't do either way).
pub fn objects_edit_changes(doc: &Document, page: usize, objects: &[usize], edit: &ObjectsEdit) -> Result<bool, EditError> {
    let s = scan(doc, page, false)?;
    Ok(match plan(doc, &s, objects, edit)? {
        Plan::Order(order) => order.iter().enumerate().any(|(i, g)| i != *g),
        Plan::Transform(list) => !list.is_empty(),
        Plan::Style(styled) => !styled.units.is_empty(),
        Plan::Delete(units) => !units.is_empty(),
    })
}

/// Arrange, align, distribute, rotate, flip, move, resize, restyle or delete objects `objects`
/// (indexes into [`page_objects`]) of page `page` (0-based). Returns whether anything changed; when nothing
/// would (already in front, already aligned), the document is left untouched.
pub fn edit_objects(doc: &mut Document, page: usize, objects: &[usize], edit: &ObjectsEdit) -> Result<bool, EditError> {
    let arranging = matches!(edit, ObjectsEdit::Arrange(_));
    let s = scan(doc, page, arranging)?;
    match plan(doc, &s, objects, edit)? {
        Plan::Order(order) => {
            if order.iter().enumerate().all(|(i, g)| i == *g) {
                return Ok(false);
            }
            write_order(doc, &s, &order)?;
        }
        Plan::Transform(list) => {
            if list.is_empty() {
                return Ok(false);
            }
            write_transforms(doc, &s, &list)?;
        }
        Plan::Style(styled) => {
            if styled.units.is_empty() {
                return Ok(false);
            }
            write_styles(doc, &s, &styled)?;
        }
        Plan::Delete(units) => {
            if units.is_empty() {
                return Ok(false);
            }
            write_deletes(doc, &s, &units)?;
        }
    }
    Ok(true)
}

/// A number for a `cm` operand: up to six decimals, no exponent.
fn number(v: f64) -> String {
    let v = if v.abs() < 5e-7 { 0.0 } else { v };
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

fn write_transforms(doc: &mut Document, s: &Scan, list: &[(usize, [f64; 6])]) -> Result<(), EditError> {
    // (position, after-before order, bytes): the end of one object comes before the start of
    // the next at the same position.
    let mut edits: Vec<(usize, u8, Vec<u8>)> = Vec::new();
    for (g, t) in list {
        if !t.iter().all(|v| v.is_finite()) {
            return Err(EditError::Invalid("the transform must be finite".into()));
        }
        let Some(group) = s.groups.get(*g) else { continue };
        for unit in s.units.get(group.units.clone()).unwrap_or_default().iter().filter(|u| u.kind.paints()) {
            if let Some(p) = &unit.problem {
                return Err(EditError::Invalid(format!("this object can't be changed: {p}")));
            }
            let inv = unit.ctm.invert().ok_or_else(|| EditError::Invalid("the object is drawn with no area".into()))?;
            let m = unit.ctm.then(&Matrix(*t)).then(&inv);
            if !m.0.iter().all(|v| v.is_finite() && v.abs() < 1e12) {
                return Err(EditError::Invalid("the transform is too large".into()));
            }
            let (Some(first), Some(last)) = (s.ops.get(unit.ops.start), unit.ops.end.checked_sub(1).and_then(|e| s.ops.get(e))) else {
                continue;
            };
            let before = format!("\nq {} cm\n", m.0.iter().map(|v| number(*v)).collect::<Vec<_>>().join(" ")).into_bytes();
            let mut after = b"\nQ\n".to_vec();
            for op in &unit.residue {
                s.op_bytes(op, &mut after);
            }
            edits.push((first.span.start, 1, before));
            edits.push((last.span.end, 0, after));
        }
    }
    edits.sort_by_key(|e| (e.0, e.1));
    let new = s.pieces.splice(edits.into_iter().map(|(at, _, bytes)| (at..at, bytes)));
    let contents = rewritten_contents(doc, &s.streams, new);
    doc.update_dict(s.page.obj, |d| d.set(b"Contents".to_vec(), contents))?;
    Ok(())
}

fn write_styles(doc: &mut Document, s: &Scan, styled: &Styled) -> Result<(), EditError> {
    let mut edits: Vec<(Range<usize>, Vec<u8>)> = Vec::new();
    for (k, state, paint) in &styled.units {
        let Some(u) = s.units.get(*k) else { continue };
        let ops = s.ops.get(u.ops.clone()).unwrap_or_default();
        let (Some(first), Some((last, body))) = (ops.first(), ops.split_last()) else { continue };
        let mut out = b"\nq ".to_vec();
        out.extend_from_slice(state);
        out.push(b'\n');
        match paint {
            Some(paint) => {
                for op in body {
                    s.op_bytes(op, &mut out);
                }
                out.extend_from_slice(paint.as_bytes());
                out.extend_from_slice(b"\nQ\n");
            }
            None => {
                for op in ops {
                    s.op_bytes(op, &mut out);
                }
                out.extend_from_slice(b"Q\n");
            }
        }
        for op in &u.residue {
            s.op_bytes(op, &mut out);
        }
        edits.push((first.span.start..last.span.end, out));
    }
    edits.sort_by_key(|e| e.0.start);
    let new = s.pieces.splice(edits);
    let contents = rewritten_contents(doc, &s.streams, new);
    doc.update_dict(s.page.obj, |d| d.set(b"Contents".to_vec(), contents))?;
    if let Some((name, opacity)) = &styled.opacity {
        // The page gets its own (possibly copied) resources; shared dictionaries stay untouched.
        let mut res = resources(doc, &s.page.dict);
        let mut states = sub_dict(doc, &res, b"ExtGState");
        let mut state = Dict::new();
        state.set(b"Type".to_vec(), Object::name("ExtGState"));
        state.set(b"CA".to_vec(), Object::Real(*opacity));
        state.set(b"ca".to_vec(), Object::Real(*opacity));
        states.set(name.clone(), Object::Dict(state));
        res.set(b"ExtGState".to_vec(), Object::Dict(states));
        doc.update_dict(s.page.obj, |d| d.set(b"Resources".to_vec(), Object::Dict(res)))?;
    }
    Ok(())
}

fn write_deletes(doc: &mut Document, s: &Scan, units: &[usize]) -> Result<(), EditError> {
    let mut edits: Vec<(Range<usize>, Vec<u8>)> = Vec::new();
    for k in units {
        let Some(u) = s.units.get(*k) else { continue };
        let ops = s.ops.get(u.ops.clone()).unwrap_or_default();
        let (Some(first), Some(last)) = (ops.first(), ops.last()) else { continue };
        // What it leaves for later content (a clip, text state) stays.
        let mut out = b"\n".to_vec();
        for op in &u.residue {
            s.op_bytes(op, &mut out);
        }
        edits.push((first.span.start..last.span.end, out));
    }
    edits.sort_by_key(|e| e.0.start);
    let new = s.pieces.splice(edits);
    let contents = rewritten_contents(doc, &s.streams, new);
    doc.update_dict(s.page.obj, |d| d.set(b"Contents".to_vec(), contents))?;
    Ok(())
}

fn write_order(doc: &mut Document, s: &Scan, order: &[usize]) -> Result<(), EditError> {
    if let Some(p) = &s.problem {
        return Err(EditError::Invalid(format!("this page can't be rearranged: {p}")));
    }
    if let Some(p) = s.units.iter().find_map(|u| u.problem.as_ref()) {
        return Err(EditError::Invalid(format!("this page can't be rearranged: {p}")));
    }
    let mut out: Vec<u8> = Vec::new();
    let mut open: Vec<usize> = Vec::new();
    for g in order {
        let Some(group) = s.groups.get(*g) else { continue };
        for unit in s.units.get(group.units.clone()).unwrap_or_default() {
            // Close the marked content this object isn't in, open what it is in.
            let keep = open.iter().zip(&unit.marked).take_while(|(a, b)| a == b).count();
            for _ in keep..open.len() {
                out.extend_from_slice(b"EMC\n");
            }
            open.truncate(keep);
            for &m in unit.marked.get(keep..).unwrap_or_default() {
                if let Some(op) = s.ops.get(m) {
                    s.op_bytes(op, &mut out);
                }
                open.push(m);
            }
            let ops = s.ops.get(unit.ops.clone()).unwrap_or_default();
            match unit.kind {
                UnitKind::Point => ops.iter().for_each(|op| s.op_bytes(op, &mut out)),
                UnitKind::EmptyMarked => {
                    if let Some(op) = ops.first() {
                        s.op_bytes(op, &mut out);
                    }
                    out.extend_from_slice(b"EMC\n");
                }
                _ => {
                    out.extend_from_slice(b"q\n");
                    for op in &unit.prefix {
                        s.op_bytes(op, &mut out);
                    }
                    for op in ops {
                        s.op_bytes(op, &mut out);
                    }
                    out.extend_from_slice(b"Q\n");
                }
            }
        }
    }
    for _ in open {
        out.extend_from_slice(b"EMC\n");
    }
    // Leave the graphics state as the content did, for whatever follows it.
    for (i, level) in s.end_state.iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(b"q\n");
        }
        for op in level {
            s.op_bytes(op, &mut out);
        }
    }
    let new = Object::Ref(doc.add(Object::Stream(Stream::flate(Dict::new(), &out))));
    let mut list: Vec<Object> = Vec::with_capacity(s.streams.len());
    list.extend(s.streams.iter().take(s.region.start).map(|(o, _)| o.clone()));
    list.push(new);
    list.extend(s.streams.iter().skip(s.region.end).map(|(o, _)| o.clone()));
    let contents = match <[Object; 1]>::try_from(list) {
        Ok([one]) => one,
        Err(many) => Object::Array(many),
    };
    doc.update_dict(s.page.obj, |d| d.set(b"Contents".to_vec(), contents))?;
    Ok(())
}
