//! Total area coverage (TAC): the most ink a page puts down at one spot, as the sum of its
//! process (C, M, Y, K) and spot ink percentages (0–400 %, more with spot inks), and the
//! regions where that sum exceeds a press limit (Acrobat's Output Preview "Total Area
//! Coverage" warning; 300 % by default).
//!
//! The page renderer only produces RGB, so this module has its own small, bounded ink
//! rasterizer: it interprets the page's content stream (and the forms it draws) and paints
//! each object's ink total into a coverage grid at a chosen resolution, opaquely, later
//! objects replacing earlier ones (knockout, no overprint).
//!
//! - **Exact** for colours given in DeviceCMYK, DeviceGray (black ink), Separation, DeviceN,
//!   ICCBased with 1 or 4 components, and Indexed over those: their component values *are*
//!   ink percentages.
//! - **Converted** for DeviceRGB, CalRGB, three-component ICCBased and Lab: a documented
//!   naive conversion without a press profile (full grey-component replacement:
//!   `K = 1 − max(R, G, B)`, `C = (1 − R − K)/(1 − K)` …, Lab via its lightness as grey). It
//!   never reaches more than 200 %, so RGB artwork that a press profile would separate into a
//!   rich black is under-reported. Each page says how many painted objects were converted.
//! - **Not measured** (counted per reason): pattern and shading fills, images in codecs the
//!   filters don't decode (DCT, JPX, JBIG2, CCITT), oversized images, unknown colour spaces
//!   and annotations. Transparency, blend modes, soft masks and overprint are ignored.
//!
//! Geometry is approximate at the grid's resolution: fills use the real outline (non-zero or
//! even-odd), strokes are covered segment by segment with square ends, text is measured as
//! the box of each shown string (font widths, ascent and descent), images are sampled per
//! cell and clipping paths count as their bounding boxes. A cell is painted when its centre
//! is inside. All work is capped (grid cells, content bytes, operators, painted objects,
//! rasterization work and nesting); a page that hits a cap says so with `truncated`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

use pdfcraft_content::{Matrix, Op, parse};
use pdfcraft_cos::{Dict, Document, ObjRef, Object, Stream};

#[cfg(test)]
mod tests;

/// The default press limit, in percent.
pub const DEFAULT_LIMIT: f64 = 300.0;
pub const MIN_LIMIT: f64 = 1.0;
pub const MAX_LIMIT: f64 = 2000.0;
/// The default grid resolution, in cells per inch.
pub const DEFAULT_DPI: f64 = 72.0;
pub const MIN_DPI: f64 = 9.0;
pub const MAX_DPI: f64 = 300.0;
/// Grid cells per page; a larger page or resolution is measured at a lower resolution.
pub const MAX_CELLS: usize = 4_000_000;
pub const DEFAULT_REGIONS: usize = 5;
pub const MAX_REGIONS: usize = 50;

const MAX_CONTENT: usize = 16 << 20;
const MAX_STREAMS: usize = 4096;
const MAX_DEPTH: usize = 16;
const MAX_STACK: usize = 256;
const MAX_OPERATORS: usize = 2_000_000;
const MAX_PAINTS: usize = 500_000;
const MAX_WORK: u64 = 600_000_000;
const MAX_PATH_POINTS: usize = 200_000;
const MAX_IMAGE_BYTES: usize = 64 << 20;
const MAX_COMPONENTS: usize = 32;
const MAX_WIDTH_RANGES: usize = 8192;
const MAX_CURVE_STEPS: usize = 32;

/// How the measurement works and what it can't see, for reports.
pub const METHOD: &str = "Ink coverage is rasterized by PdfCraft's own content interpreter (the page renderer is RGB-only). \
CMYK, Gray (as black), Separation, DeviceN and Indexed colours are exact ink values; RGB and Lab colours use a naive \
full-GCR conversion without a press profile (never above 200%, so RGB rich blacks are under-reported). Patterns, \
shadings, DCT/JPX/JBIG2/CCITT images and annotations are not measured; transparency and overprint are ignored; \
clipping paths count as their bounding boxes and text as string boxes.";

/// What to measure.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// The press limit in percent: cells above it are reported.
    pub limit: f64,
    /// Requested grid resolution (cells per inch).
    pub dpi: f64,
    /// How many of the worst regions to return per page.
    pub max_regions: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self { limit: DEFAULT_LIMIT, dpi: DEFAULT_DPI, max_regions: DEFAULT_REGIONS }
    }
}

impl Options {
    pub fn validate(&self) -> Result<(), String> {
        if !self.limit.is_finite() || !(MIN_LIMIT..=MAX_LIMIT).contains(&self.limit) {
            return Err(format!("limit must be a percentage between {MIN_LIMIT} and {MAX_LIMIT}"));
        }
        if !self.dpi.is_finite() || !(MIN_DPI..=MAX_DPI).contains(&self.dpi) {
            return Err(format!("dpi must be between {MIN_DPI} and {MAX_DPI}"));
        }
        if !(1..=MAX_REGIONS).contains(&self.max_regions) {
            return Err(format!("max_regions must be between 1 and {MAX_REGIONS}"));
        }
        Ok(())
    }
}

/// A connected area above the limit.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    /// `[x0, y0, x1, y1]` in PDF user space.
    pub bbox: [f64; 4],
    /// The highest coverage inside, in percent.
    pub max: f64,
    /// Area above the limit, in square points.
    pub area: f64,
}

/// One page's coverage.
#[derive(Clone, Debug, PartialEq)]
pub struct PageCoverage {
    /// 0-based page index.
    pub page: usize,
    /// The measured area (the crop box) in user space.
    pub crop: [f64; 4],
    /// The highest total coverage on the page, in percent.
    pub max: f64,
    /// Area above the limit, in square points.
    pub over_area: f64,
    /// That area as a percentage of the page area.
    pub over_percent: f64,
    /// The worst regions (highest coverage first, then largest), at most `max_regions`.
    pub regions: Vec<Region>,
    /// How many separate regions exceed the limit.
    pub region_count: usize,
    /// The resolution used (lower than requested when the page would exceed [`MAX_CELLS`]).
    pub dpi: f64,
    /// Painted objects whose ink is exact (CMYK, Gray, Separation, DeviceN, Indexed of those).
    pub exact_paints: usize,
    /// Painted objects whose ink came from the RGB/Lab → CMYK approximation.
    pub converted_paints: usize,
    /// Objects that could not be measured, by reason.
    pub unmeasured: BTreeMap<&'static str, usize>,
    /// A cap was reached; part of the page was not measured.
    pub truncated: bool,
}

impl PageCoverage {
    /// Whether any part of the page is over the limit.
    pub fn flagged(&self) -> bool {
        self.region_count > 0
    }
}

/// Measure 0-based page `index`.
pub fn page_coverage(doc: &Document, index: usize, options: &Options) -> Result<PageCoverage, String> {
    options.validate()?;
    let pages = pdfcraft_model::pages(doc);
    let page = pages.get(index).ok_or_else(|| format!("page {} does not exist; the document has {} pages", index.saturating_add(1), pages.len()))?;
    let crop = page.crop(doc);
    let (width, height) = (crop[2] - crop[0], crop[3] - crop[1]);
    if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0) {
        return Err("the page has an empty or invalid crop box".into());
    }
    // Choose the resolution: requested, lowered until the grid fits the cell budget.
    let mut dpi = options.dpi;
    let cells_at = |dpi: f64| ((width * dpi / 72.0).ceil().max(1.0), (height * dpi / 72.0).ceil().max(1.0));
    let (w, h) = cells_at(dpi);
    if w * h > MAX_CELLS as f64 {
        dpi *= (MAX_CELLS as f64 / (w * h)).sqrt() * 0.999;
    }
    let (mut w, mut h) = cells_at(dpi);
    // Extremely thin pages: keep each side within the budget too.
    w = w.min(MAX_CELLS as f64);
    h = h.min((MAX_CELLS as f64 / w).floor().max(1.0));
    let (gw, gh) = (w as usize, h as usize);
    let cell = 72.0 / dpi;
    let mut grid = Grid { w: gw, h: gh, cells: vec![0.0; gw.saturating_mul(gh)], work: 0, truncated: false, crossings: Vec::new() };
    let to_grid = Matrix([1.0 / cell, 0.0, 0.0, 1.0 / cell, -crop[0] / cell, -crop[1] / cell]);

    let mut walker = Walker {
        doc,
        grid: &mut grid,
        to_grid,
        cell,
        seen: HashSet::new(),
        bytes: 0,
        streams: 0,
        operators: 0,
        paints: 0,
        exact: 0,
        converted: 0,
        unmeasured: BTreeMap::new(),
        truncated: false,
        fonts: HashMap::new(),
    };
    let resources = page.dict.get(b"Resources").map(|o| doc.resolve(o));
    let empty = Dict::new();
    let resources = resources.as_ref().and_then(|o| o.as_dict()).unwrap_or(&empty);
    let gs = Gs::new([0.0, 0.0, gw as f64, gh as f64]);
    if let Some(contents) = page.dict.get(b"Contents") {
        let data = walker.content(contents);
        walker.run(&data, resources, gs, 0);
    }
    if page.dict.get(b"Annots").map(|o| doc.resolve(o)).and_then(|o| o.as_array().map(Vec::len)).is_some_and(|n| n > 0) {
        walker.unmeasure("annotations");
    }
    let (exact, converted, unmeasured, mut truncated) = (walker.exact, walker.converted, walker.unmeasured, walker.truncated);
    truncated |= grid.truncated;

    let max = grid.cells.iter().copied().fold(0.0f32, f32::max);
    let (regions, region_count, over_cells) = regions(&grid, options.limit as f32, options.max_regions);
    let cell_area = cell * cell;
    let over_area = over_cells as f64 * cell_area;
    let page_area = width * height;
    let to_user = |r: [usize; 4]| {
        [
            crop[0] + r[0] as f64 * cell,
            crop[1] + r[1] as f64 * cell,
            (crop[0] + r[2] as f64 * cell).min(crop[2]),
            (crop[1] + r[3] as f64 * cell).min(crop[3]),
        ]
    };
    Ok(PageCoverage {
        page: index,
        crop,
        max: round(f64::from(max)),
        over_area: round(over_area),
        over_percent: round((over_area / page_area * 100.0).min(100.0)),
        regions: regions
            .into_iter()
            .map(|(bbox, max, cells)| Region { bbox: to_user(bbox), max: round(f64::from(max)), area: round(cells as f64 * cell_area) })
            .collect(),
        region_count,
        dpi: round(dpi),
        exact_paints: exact,
        converted_paints: converted,
        unmeasured,
        truncated,
    })
}

fn round(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Connected (8-neighbour) regions of cells above `limit`: the worst `keep`, the number of
/// regions and the number of cells above the limit.
/// A found region: cell bounds, highest value, cell count.
type Found = ([usize; 4], f32, usize);

fn regions(grid: &Grid, limit: f32, keep: usize) -> (Vec<Found>, usize, usize) {
    let (w, h) = (grid.w, grid.h);
    let mut visited = vec![false; grid.cells.len()];
    let mut out: Vec<Found> = Vec::new();
    let (mut count, mut over) = (0usize, 0usize);
    let mut stack = Vec::new();
    let order = |a: &Found, b: &Found| b.1.total_cmp(&a.1).then(b.2.cmp(&a.2));
    for start in 0..grid.cells.len() {
        if grid.cells.get(start).is_none_or(|v| *v <= limit) || visited.get(start).copied().unwrap_or(true) {
            continue;
        }
        count += 1;
        let (mut bbox, mut max, mut cells) = ([usize::MAX, usize::MAX, 0, 0], 0.0f32, 0usize);
        if let Some(v) = visited.get_mut(start) {
            *v = true;
        }
        stack.push(start);
        while let Some(at) = stack.pop() {
            let (x, y) = (at % w, at / w);
            let value = grid.cells.get(at).copied().unwrap_or(0.0);
            cells += 1;
            max = max.max(value);
            bbox = [bbox[0].min(x), bbox[1].min(y), bbox[2].max(x + 1), bbox[3].max(y + 1)];
            for (dx, dy) in [(-1i64, -1i64), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
                let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                    continue;
                }
                let next = ny as usize * w + nx as usize;
                if grid.cells.get(next).is_some_and(|v| *v > limit)
                    && let Some(seen) = visited.get_mut(next)
                    && !*seen
                {
                    *seen = true;
                    stack.push(next);
                }
            }
        }
        over += cells;
        out.push((bbox, max, cells));
        if out.len() > keep.saturating_mul(4).max(64) {
            out.sort_by(order);
            out.truncate(keep);
        }
    }
    out.sort_by(order);
    out.truncate(keep);
    (out, count, over)
}

// ---- colour ---------------------------------------------------------------------------------

/// An object's ink total in percent, and whether it came from a colour conversion.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Ink {
    total: f32,
    converted: bool,
}

#[derive(Clone, Debug)]
enum Space {
    Gray,
    Rgb,
    Cmyk,
    Lab,
    /// Ink weight per unit tint: 0 for `None`, 4 for `All` (every process plate), else 1.
    Separation(f64),
    /// Per-component weights (0 for `None` colorants).
    DeviceN(Vec<f64>),
    Indexed {
        base: Box<Space>,
        hival: usize,
        lookup: Vec<u8>,
    },
    Pattern,
    Unknown(usize),
}

fn clamp01(v: f64) -> f64 {
    if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }
}

impl Space {
    fn components(&self) -> usize {
        match self {
            Space::Gray | Space::Separation(_) | Space::Indexed { .. } => 1,
            Space::Rgb | Space::Lab => 3,
            Space::Cmyk => 4,
            Space::DeviceN(w) => w.len(),
            Space::Pattern => 0,
            Space::Unknown(n) => *n,
        }
    }

    fn initial(&self) -> Vec<f64> {
        match self {
            Space::Gray | Space::Rgb | Space::Indexed { .. } => vec![0.0; self.components()],
            Space::Lab => vec![0.0, 0.0, 0.0],
            Space::Cmyk => vec![0.0, 0.0, 0.0, 1.0],
            Space::Separation(_) | Space::DeviceN(_) => vec![1.0; self.components()],
            Space::Pattern | Space::Unknown(_) => Vec::new(),
        }
    }

    /// Ink for colour components `c`, or `None` when this space can't be measured.
    fn ink(&self, c: &[f64]) -> Option<Ink> {
        let at = |i: usize| c.get(i).copied().unwrap_or(0.0);
        let exact = |total: f64| Some(Ink { total: (total * 100.0) as f32, converted: false });
        match self {
            Space::Gray => exact(1.0 - clamp01(at(0))),
            Space::Cmyk => exact((0..4).map(|i| clamp01(at(i))).sum()),
            Space::Separation(weight) => exact(clamp01(at(0)) * weight),
            Space::DeviceN(weights) => exact(weights.iter().enumerate().map(|(i, w)| clamp01(at(i)) * w).sum()),
            Space::Rgb => Some(Ink { total: (rgb_ink([clamp01(at(0)), clamp01(at(1)), clamp01(at(2))]) * 100.0) as f32, converted: true }),
            Space::Lab => Some(Ink { total: ((1.0 - clamp01(at(0) / 100.0)) * 100.0) as f32, converted: true }),
            Space::Indexed { base, hival, lookup } => {
                let index = at(0);
                let index = if index.is_finite() { index.round().clamp(0.0, *hival as f64) as usize } else { 0 };
                let n = base.components();
                let start = index.checked_mul(n)?;
                let entry = lookup.get(start..start.checked_add(n)?)?;
                let mut comps = [0.0; MAX_COMPONENTS];
                for (slot, byte) in comps.iter_mut().zip(entry) {
                    *slot = f64::from(*byte) / 255.0;
                }
                if matches!(**base, Space::Lab) {
                    comps[0] *= 100.0;
                }
                base.ink(comps.get(..n)?)
            }
            Space::Pattern | Space::Unknown(_) => None,
        }
    }
}

/// The naive RGB → CMYK conversion (full grey-component replacement), as a 0–2 total.
fn rgb_ink([r, g, b]: [f64; 3]) -> f64 {
    let k = 1.0 - r.max(g).max(b);
    if k >= 1.0 - 1e-9 {
        return 1.0;
    }
    let ink = |v: f64| (1.0 - v - k) / (1.0 - k);
    ink(r) + ink(g) + ink(b) + k
}

#[derive(Clone, Debug)]
struct Paint {
    space: Rc<Space>,
    comps: Vec<f64>,
    /// A pattern colour (`/P scn`): not measured.
    pattern: bool,
}

impl Paint {
    fn device(space: Space, comps: Vec<f64>) -> Self {
        Self { space: Rc::new(space), comps, pattern: false }
    }
    fn ink(&self) -> Option<Ink> {
        if self.pattern { None } else { self.space.ink(&self.comps) }
    }
}

// ---- fonts ----------------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Font {
    two_byte: bool,
    first: i64,
    widths: Vec<f64>,
    ranges: Vec<(u32, u32, f64)>,
    default_width: f64,
    /// Glyph-space units to text-space units (0.001, or a Type 3 font's matrix).
    scale: f64,
    ascent: f64,
    descent: f64,
}

impl Font {
    fn fallback() -> Self {
        Self { two_byte: false, first: 0, widths: Vec::new(), ranges: Vec::new(), default_width: 500.0, scale: 0.001, ascent: 0.8, descent: -0.2 }
    }

    fn width(&self, code: u32) -> f64 {
        let w = if self.two_byte {
            self.ranges.iter().find(|(a, b, _)| (*a..=*b).contains(&code)).map_or(self.default_width, |r| r.2)
        } else {
            usize::try_from(i64::from(code) - self.first).ok().and_then(|i| self.widths.get(i).copied()).unwrap_or(self.default_width)
        };
        let w = w * self.scale;
        if w.is_finite() { w.clamp(-100.0, 100.0) } else { 0.0 }
    }
}

// ---- grid -----------------------------------------------------------------------------------

type P = (f64, f64);

struct Grid {
    w: usize,
    h: usize,
    cells: Vec<f32>,
    work: u64,
    truncated: bool,
    crossings: Vec<(f64, i32)>,
}

impl Grid {
    fn spend(&mut self, cost: u64) -> bool {
        self.work = self.work.saturating_add(cost);
        if self.work > MAX_WORK {
            self.truncated = true;
            false
        } else {
            true
        }
    }

    /// Cell index range whose centres lie in `[lo, hi)`, within `0..n`.
    fn span(lo: f64, hi: f64, n: usize) -> (usize, usize) {
        let a = (lo - 0.5).ceil().max(0.0);
        let b = (hi - 0.5).ceil().max(0.0);
        let n = n as f64;
        ((a.min(n)) as usize, (b.min(n)) as usize)
    }

    /// Fill the closed polygons `subpaths` (grid coordinates) inside `clip`.
    fn fill(&mut self, subpaths: &[Vec<P>], even_odd: bool, clip: [f64; 4], value: f32) {
        let mut bbox = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        let mut edges = 0u64;
        for sp in subpaths {
            for &(x, y) in sp {
                bbox = [bbox[0].min(x), bbox[1].min(y), bbox[2].max(x), bbox[3].max(y)];
            }
            edges = edges.saturating_add(sp.len() as u64);
        }
        if !bbox.iter().all(|v| v.is_finite()) {
            return;
        }
        let (x0, x1) = Self::span(bbox[0].max(clip[0]), bbox[2].min(clip[2]), self.w);
        let (y0, y1) = Self::span(bbox[1].max(clip[1]), bbox[3].min(clip[3]), self.h);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let (cx0, cx1) = (x0, x1);
        if !self.spend(((y1 - y0) as u64).saturating_mul(edges.saturating_add(1))) {
            return;
        }
        let mut crossings = std::mem::take(&mut self.crossings);
        for row in y0..y1 {
            let yc = row as f64 + 0.5;
            crossings.clear();
            for sp in subpaths {
                let n = sp.len();
                if n < 2 {
                    continue;
                }
                for i in 0..n {
                    let (Some(&a), Some(&b)) = (sp.get(i), sp.get((i + 1) % n)) else { continue };
                    if a.1 == b.1 {
                        continue;
                    }
                    let (lo, hi, dir) = if a.1 < b.1 { (a, b, 1) } else { (b, a, -1) };
                    if yc >= lo.1 && yc < hi.1 {
                        let x = lo.0 + (yc - lo.1) * (hi.0 - lo.0) / (hi.1 - lo.1);
                        crossings.push((x, dir));
                    }
                }
            }
            crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut winding = 0i32;
            let mut start = 0.0;
            for &(x, dir) in crossings.iter() {
                let was = if even_odd { winding & 1 != 0 } else { winding != 0 };
                winding = winding.saturating_add(dir);
                let now = if even_odd { winding & 1 != 0 } else { winding != 0 };
                if !was && now {
                    start = x;
                } else if was && !now {
                    let (a, b) = Self::span(start, x, self.w);
                    let (a, b) = (a.max(cx0), b.min(cx1));
                    if a < b {
                        self.work = self.work.saturating_add((b - a) as u64);
                        let base = row * self.w;
                        if let Some(cells) = self.cells.get_mut(base + a..base + b) {
                            cells.fill(value);
                        }
                    }
                }
            }
        }
        self.crossings = crossings;
    }
}

// ---- interpreter ----------------------------------------------------------------------------

#[derive(Clone)]
struct Gs {
    ctm: Matrix,
    /// Clip bounding box in grid coordinates.
    clip: [f64; 4],
    fill: Paint,
    stroke: Paint,
    line_width: f64,
    font: Option<Rc<Font>>,
    size: f64,
    char_spacing: f64,
    word_spacing: f64,
    scaling: f64,
    leading: f64,
    rise: f64,
    render: i64,
}

impl Gs {
    fn new(clip: [f64; 4]) -> Self {
        Self {
            ctm: Matrix::IDENTITY,
            clip,
            fill: Paint::device(Space::Gray, vec![0.0]),
            stroke: Paint::device(Space::Gray, vec![0.0]),
            line_width: 1.0,
            font: None,
            size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            scaling: 1.0,
            leading: 0.0,
            rise: 0.0,
            render: 0,
        }
    }
}

struct Walker<'a> {
    doc: &'a Document,
    grid: &'a mut Grid,
    to_grid: Matrix,
    cell: f64,
    seen: HashSet<ObjRef>,
    bytes: usize,
    streams: usize,
    operators: usize,
    paints: usize,
    exact: usize,
    converted: usize,
    unmeasured: BTreeMap<&'static str, usize>,
    truncated: bool,
    fonts: HashMap<ObjRef, Rc<Font>>,
}

fn intersect(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])]
}

fn finite(m: &Matrix) -> bool {
    m.0.iter().all(|v| v.is_finite() && v.abs() < 1e12)
}

impl Walker<'_> {
    fn unmeasure(&mut self, why: &'static str) {
        let n = self.unmeasured.entry(why).or_insert(0);
        *n = n.saturating_add(1);
    }

    /// The decoded bytes of a content stream or array of streams, within the page budget.
    fn content(&mut self, object: &Object) -> Vec<u8> {
        let o = self.doc.resolve(object);
        let parts: Vec<Object> = match o.as_ref() {
            Object::Array(a) => a.iter().take(MAX_STREAMS).cloned().collect(),
            other => vec![other.clone()],
        };
        let mut joined = Vec::new();
        for part in parts {
            let s = self.doc.resolve(&part);
            let Object::Stream(s) = s.as_ref() else { continue };
            if !self.budget_stream() {
                break;
            }
            let remaining = MAX_CONTENT.saturating_sub(self.bytes);
            match s.decoded_within(remaining) {
                Ok(data) => {
                    self.bytes = self.bytes.saturating_add(data.len());
                    joined.extend(data);
                    joined.push(b'\n');
                }
                Err(_) => self.unmeasure("unreadable content streams"),
            }
        }
        joined
    }

    fn budget_stream(&mut self) -> bool {
        self.streams = self.streams.saturating_add(1);
        if self.streams > MAX_STREAMS || self.bytes >= MAX_CONTENT {
            self.truncated = true;
            false
        } else {
            true
        }
    }

    fn paint_counted(&mut self, ink: Option<Ink>, why: &'static str) -> Option<f32> {
        self.paints = self.paints.saturating_add(1);
        if self.paints > MAX_PAINTS {
            self.truncated = true;
            return None;
        }
        match ink {
            Some(i) => {
                if i.converted {
                    self.converted = self.converted.saturating_add(1);
                } else {
                    self.exact = self.exact.saturating_add(1);
                }
                Some(if i.total.is_finite() { i.total.max(0.0) } else { 0.0 })
            }
            None => {
                self.unmeasure(why);
                None
            }
        }
    }

    fn paint_reason(paint: &Paint) -> &'static str {
        if paint.pattern || matches!(*paint.space, Space::Pattern) { "pattern fills" } else { "unsupported colour spaces" }
    }

    fn device(&self, gs: &Gs) -> Matrix {
        gs.ctm.then(&self.to_grid)
    }

    fn colour_space(&self, o: &Object, resources: &Dict, depth: usize) -> Space {
        if depth > 4 {
            return Space::Unknown(1);
        }
        let o = self.doc.resolve(o);
        match o.as_ref() {
            Object::Name(n) => match n.as_slice() {
                b"DeviceGray" | b"G" | b"CalGray" => Space::Gray,
                b"DeviceRGB" | b"RGB" | b"CalRGB" => Space::Rgb,
                b"DeviceCMYK" | b"CMYK" => Space::Cmyk,
                b"Pattern" => Space::Pattern,
                other => {
                    let named =
                        resources.get(b"ColorSpace").map(|d| self.doc.resolve(d)).and_then(|d| d.as_dict().and_then(|d| d.get(other).cloned()));
                    match named {
                        Some(v) => self.colour_space(&v, &Dict::new(), depth + 1),
                        None => Space::Unknown(1),
                    }
                }
            },
            Object::Array(a) => {
                let family = a.first().map(|f| self.doc.resolve(f)).and_then(|f| f.as_name().map(<[u8]>::to_vec)).unwrap_or_default();
                let arg = |i: usize| a.get(i).map(|o| self.doc.resolve(o));
                match family.as_slice() {
                    b"DeviceGray" | b"CalGray" | b"G" => Space::Gray,
                    b"DeviceRGB" | b"CalRGB" | b"RGB" => Space::Rgb,
                    b"DeviceCMYK" | b"CMYK" => Space::Cmyk,
                    b"Lab" => Space::Lab,
                    b"ICCBased" => {
                        let n = arg(1).and_then(|s| match s.as_ref() {
                            Object::Stream(s) => s.dict.get(b"N").and_then(|n| self.doc.resolve(n).as_int()),
                            _ => None,
                        });
                        match n {
                            Some(1) => Space::Gray,
                            Some(3) => Space::Rgb,
                            Some(4) => Space::Cmyk,
                            Some(n) => Space::Unknown(usize::try_from(n).unwrap_or(1).clamp(1, MAX_COMPONENTS)),
                            None => Space::Unknown(1),
                        }
                    }
                    b"Separation" => match arg(1).as_deref().and_then(Object::as_name) {
                        Some(b"None") => Space::Separation(0.0),
                        Some(b"All") => Space::Separation(4.0),
                        _ => Space::Separation(1.0),
                    },
                    b"DeviceN" => match arg(1).as_deref().and_then(Object::as_array) {
                        Some(names) if !names.is_empty() && names.len() <= MAX_COMPONENTS => {
                            Space::DeviceN(names.iter().map(|n| if self.doc.resolve(n).as_name() == Some(b"None") { 0.0 } else { 1.0 }).collect())
                        }
                        _ => Space::Unknown(1),
                    },
                    b"Indexed" | b"I" => {
                        let base = a.get(1).map_or(Space::Unknown(1), |b| self.colour_space(b, resources, depth + 1));
                        if matches!(base, Space::Indexed { .. } | Space::Pattern | Space::Unknown(_)) {
                            return Space::Unknown(1);
                        }
                        let hival = arg(2).and_then(|h| h.as_int()).unwrap_or(0).clamp(0, 255) as usize;
                        let want = (hival + 1).saturating_mul(base.components());
                        let lookup = match arg(3).as_deref() {
                            Some(Object::String(s)) => s.bytes.iter().take(want).copied().collect(),
                            Some(Object::Stream(s)) => {
                                s.decoded_within(want.max(1).saturating_mul(2)).unwrap_or_default().into_iter().take(want).collect()
                            }
                            _ => Vec::new(),
                        };
                        Space::Indexed { base: Box::new(base), hival, lookup }
                    }
                    b"Pattern" => Space::Pattern,
                    _ => Space::Unknown(1),
                }
            }
            _ => Space::Unknown(1),
        }
    }

    fn font(&mut self, name: &[u8], resources: &Dict) -> Rc<Font> {
        let entry = resources.get(b"Font").map(|d| self.doc.resolve(d)).and_then(|d| d.as_dict().and_then(|d| d.get(name).cloned()));
        let Some(entry) = entry else { return Rc::new(Font::fallback()) };
        if let Some(r) = entry.as_ref()
            && let Some(f) = self.fonts.get(&r)
        {
            return f.clone();
        }
        let font = Rc::new(self.load_font(&entry));
        if let Some(r) = entry.as_ref()
            && self.fonts.len() < 4096
        {
            self.fonts.insert(r, font.clone());
        }
        font
    }

    fn load_font(&self, entry: &Object) -> Font {
        let d = self.doc.resolve(entry);
        let Some(d) = d.as_dict() else { return Font::fallback() };
        let num = |o: &Object| self.doc.resolve(o).as_f64().filter(|v| v.is_finite());
        let mut font = Font::fallback();
        let descriptor_of = |d: &Dict| d.get(b"FontDescriptor").map(|o| self.doc.resolve(o)).and_then(|o| o.as_dict().cloned());
        let descriptor = if d.name(b"Subtype") == Some(b"Type0") {
            font.two_byte = true;
            font.default_width = 1000.0;
            let descendant = d
                .get(b"DescendantFonts")
                .map(|o| self.doc.resolve(o))
                .and_then(|a| a.as_array().and_then(|a| a.first().cloned()))
                .map(|o| self.doc.resolve(&o))
                .and_then(|o| o.as_dict().cloned())
                .unwrap_or_default();
            if let Some(dw) = descendant.get(b"DW").and_then(num) {
                font.default_width = dw;
            }
            if let Some(w) = descendant.get(b"W").map(|o| self.doc.resolve(o)).and_then(|o| o.as_array().cloned()) {
                let mut i = 0;
                while i < w.len() && font.ranges.len() < MAX_WIDTH_RANGES {
                    let Some(first) = w.get(i).and_then(num) else { break };
                    let first = first.clamp(0.0, f64::from(u32::MAX)) as u32;
                    match w.get(i + 1).map(|o| self.doc.resolve(o)) {
                        Some(next) if next.as_array().is_some() => {
                            for (k, v) in next.as_array().into_iter().flatten().enumerate().take(MAX_WIDTH_RANGES) {
                                if let Some(v) = num(v)
                                    && font.ranges.len() < MAX_WIDTH_RANGES
                                {
                                    let code = first.saturating_add(k as u32);
                                    font.ranges.push((code, code, v));
                                }
                            }
                            i += 2;
                        }
                        Some(next) => {
                            let (Some(last), Some(v)) = (next.as_f64(), w.get(i + 2).and_then(num)) else { break };
                            font.ranges.push((first, last.clamp(0.0, f64::from(u32::MAX)) as u32, v));
                            i += 3;
                        }
                        None => break,
                    }
                }
            }
            descriptor_of(&descendant)
        } else {
            font.first = d.get(b"FirstChar").and_then(|o| self.doc.resolve(o).as_int()).unwrap_or(0);
            if let Some(w) = d.get(b"Widths").map(|o| self.doc.resolve(o)).and_then(|o| o.as_array().cloned()) {
                font.widths = w.iter().take(256).map(|v| num(v).unwrap_or(0.0)).collect();
            }
            if d.name(b"Subtype") == Some(b"Type3") {
                font.scale = d
                    .get(b"FontMatrix")
                    .map(|o| self.doc.resolve(o))
                    .and_then(|m| m.as_array().and_then(|a| a.first().and_then(num)))
                    .unwrap_or(0.001);
            }
            descriptor_of(d)
        };
        if let Some(desc) = descriptor {
            if let Some(m) = desc.get(b"MissingWidth").and_then(num)
                && !font.two_byte
            {
                font.default_width = m;
            }
            let ascent = desc.get(b"Ascent").and_then(num).unwrap_or(0.0) / 1000.0;
            let descent = desc.get(b"Descent").and_then(num).unwrap_or(0.0) / 1000.0;
            if ascent > 0.0 && ascent <= 4.0 {
                font.ascent = ascent;
            }
            if (-4.0..0.0).contains(&descent) {
                font.descent = descent;
            }
        }
        font
    }

    /// Interpret `data` with `resources`, starting from `gs`.
    fn run(&mut self, data: &[u8], resources: &Dict, initial: Gs, depth: usize) {
        let mut gs = initial;
        let mut stack: Vec<Gs> = Vec::new();
        let mut path: Vec<Vec<P>> = Vec::new();
        let mut closed: Vec<bool> = Vec::new();
        let mut points = 0usize;
        let mut current: Option<P> = None;
        let mut pending_clip = false;
        let mut tm = Matrix::IDENTITY;
        let mut tlm = Matrix::IDENTITY;
        for op in parse(data).ops {
            self.operators = self.operators.saturating_add(1);
            if self.operators > MAX_OPERATORS || self.truncated || self.grid.truncated {
                self.truncated = true;
                return;
            }
            let device = self.device(&gs);
            let mut add = |p: P, new: bool, path: &mut Vec<Vec<P>>, closed: &mut Vec<bool>| {
                if !(p.0.is_finite() && p.1.is_finite()) {
                    return;
                }
                points += 1;
                if points > MAX_PATH_POINTS {
                    return;
                }
                if new || path.is_empty() {
                    path.push(Vec::new());
                    closed.push(false);
                }
                if let Some(sp) = path.last_mut() {
                    sp.push(p);
                }
            };
            match op.op.as_slice() {
                b"q" => {
                    if stack.len() >= MAX_STACK {
                        self.truncated = true;
                        return;
                    }
                    stack.push(gs.clone());
                }
                b"Q" => {
                    if let Some(g) = stack.pop() {
                        gs = g;
                    }
                }
                b"cm" => {
                    if let Some(m) = Matrix::from_operands(&op.operands) {
                        let next = m.then(&gs.ctm);
                        if finite(&next) {
                            gs.ctm = next;
                        }
                    }
                }
                b"w" => {
                    if let Some(w) = op.num(0).filter(|w| w.is_finite()) {
                        gs.line_width = w.abs().min(1e6);
                    }
                }
                b"m" => {
                    if let Some([x, y]) = op.nums::<2>() {
                        let p = device.apply(x, y);
                        add(p, true, &mut path, &mut closed);
                        current = Some(p);
                    }
                }
                b"l" => {
                    if let Some([x, y]) = op.nums::<2>() {
                        let p = device.apply(x, y);
                        add(p, current.is_none(), &mut path, &mut closed);
                        current = Some(p);
                    }
                }
                b"c" | b"v" | b"y" => {
                    let Some(a) = current else { continue };
                    let ctrl = match op.op.as_slice() {
                        b"c" => op.nums::<6>().map(|[x1, y1, x2, y2, x3, y3]| (device.apply(x1, y1), device.apply(x2, y2), device.apply(x3, y3))),
                        b"v" => op.nums::<4>().map(|[x2, y2, x3, y3]| (a, device.apply(x2, y2), device.apply(x3, y3))),
                        _ => op.nums::<4>().map(|[x1, y1, x3, y3]| {
                            let d = device.apply(x3, y3);
                            (device.apply(x1, y1), d, d)
                        }),
                    };
                    let Some((b, c, d)) = ctrl else { continue };
                    let dist = |p: P, q: P| ((q.0 - p.0).powi(2) + (q.1 - p.1).powi(2)).sqrt();
                    let length = dist(a, b) + dist(b, c) + dist(c, d);
                    let steps = if length.is_finite() { (length.sqrt() * 2.0).ceil().clamp(1.0, MAX_CURVE_STEPS as f64) as usize } else { 1 };
                    for i in 1..=steps {
                        let t = i as f64 / steps as f64;
                        let u = 1.0 - t;
                        let p = (
                            u * u * u * a.0 + 3.0 * u * u * t * b.0 + 3.0 * u * t * t * c.0 + t * t * t * d.0,
                            u * u * u * a.1 + 3.0 * u * u * t * b.1 + 3.0 * u * t * t * c.1 + t * t * t * d.1,
                        );
                        add(p, false, &mut path, &mut closed);
                    }
                    current = Some(d);
                }
                b"h" => {
                    if let Some(c) = closed.last_mut() {
                        *c = true;
                    }
                    current = path.last().and_then(|sp| sp.first().copied());
                }
                b"re" => {
                    if let Some([x, y, w, h]) = op.nums::<4>() {
                        let corners = [(x, y), (x + w, y), (x + w, y + h), (x, y + h)];
                        for (i, (px, py)) in corners.into_iter().enumerate() {
                            add(device.apply(px, py), i == 0, &mut path, &mut closed);
                        }
                        if let Some(c) = closed.last_mut() {
                            *c = true;
                        }
                        current = Some(device.apply(x, y));
                    }
                }
                b"W" | b"W*" => pending_clip = true,
                b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" | b"S" | b"s" | b"n" => {
                    if points > MAX_PATH_POINTS {
                        self.truncated = true;
                    }
                    let name = op.op.as_slice();
                    if matches!(name, b"s" | b"b" | b"b*")
                        && let Some(c) = closed.last_mut()
                    {
                        *c = true;
                    }
                    if matches!(name, b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*") {
                        let even_odd = matches!(name, b"f*" | b"B*" | b"b*");
                        if let Some(v) = self.paint_counted(gs.fill.ink(), Self::paint_reason(&gs.fill)) {
                            self.grid.fill(&path, even_odd, gs.clip, v);
                        }
                    }
                    if matches!(name, b"S" | b"s" | b"B" | b"B*" | b"b" | b"b*")
                        && let Some(v) = self.paint_counted(gs.stroke.ink(), Self::paint_reason(&gs.stroke))
                    {
                        self.stroke(&path, &closed, &gs, v);
                    }
                    if pending_clip {
                        let mut bbox = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
                        for &(x, y) in path.iter().flatten() {
                            bbox = [bbox[0].min(x), bbox[1].min(y), bbox[2].max(x), bbox[3].max(y)];
                        }
                        gs.clip = if path.iter().flatten().next().is_some() { intersect(gs.clip, bbox) } else { [0.0, 0.0, 0.0, 0.0] };
                        pending_clip = false;
                    }
                    path.clear();
                    closed.clear();
                    points = 0;
                    current = None;
                }
                // Colour.
                b"g" | b"G" | b"rg" | b"RG" | b"k" | b"K" => {
                    let (space, n) = match op.op.as_slice() {
                        b"g" | b"G" => (Space::Gray, 1),
                        b"rg" | b"RG" => (Space::Rgb, 3),
                        _ => (Space::Cmyk, 4),
                    };
                    let comps: Vec<f64> = op.operands.iter().take(n).map(|o| o.as_f64().unwrap_or(0.0)).collect();
                    let paint = Paint::device(space, comps);
                    if op.op.first().is_some_and(u8::is_ascii_lowercase) {
                        gs.fill = paint;
                    } else {
                        gs.stroke = paint;
                    }
                }
                b"cs" | b"CS" => {
                    if let Some(o) = op.operands.first() {
                        let space = self.colour_space(o, resources, 0);
                        let comps = space.initial();
                        let paint = Paint { space: Rc::new(space), comps, pattern: false };
                        if op.is("cs") {
                            gs.fill = paint;
                        } else {
                            gs.stroke = paint;
                        }
                    }
                }
                b"sc" | b"scn" | b"SC" | b"SCN" => {
                    let target = if op.op.first() == Some(&b's') { &mut gs.fill } else { &mut gs.stroke };
                    let pattern = op.operands.last().is_some_and(|o| o.as_name().is_some()) || matches!(*target.space, Space::Pattern);
                    let n = target.space.components().min(MAX_COMPONENTS);
                    target.comps = op.operands.iter().filter_map(Object::as_f64).take(n).collect();
                    target.pattern = pattern;
                }
                // Text.
                b"BT" => {
                    tm = Matrix::IDENTITY;
                    tlm = Matrix::IDENTITY;
                }
                b"Tf" => {
                    if let Some(name) = op.name(0) {
                        gs.font = Some(self.font(name, resources));
                    }
                    if let Some(size) = op.num(1).filter(|s| s.is_finite()) {
                        gs.size = size.clamp(-1e5, 1e5);
                    }
                }
                b"Tc" => gs.char_spacing = op.num(0).filter(|v| v.is_finite()).unwrap_or(0.0).clamp(-1e5, 1e5),
                b"Tw" => gs.word_spacing = op.num(0).filter(|v| v.is_finite()).unwrap_or(0.0).clamp(-1e5, 1e5),
                b"Tz" => gs.scaling = op.num(0).filter(|v| v.is_finite()).unwrap_or(100.0).clamp(-1e5, 1e5) / 100.0,
                b"TL" => gs.leading = op.num(0).filter(|v| v.is_finite()).unwrap_or(0.0).clamp(-1e5, 1e5),
                b"Ts" => gs.rise = op.num(0).filter(|v| v.is_finite()).unwrap_or(0.0).clamp(-1e5, 1e5),
                b"Tr" => gs.render = op.operands.first().and_then(Object::as_int).unwrap_or(0),
                b"Td" | b"TD" => {
                    if let Some([x, y]) = op.nums::<2>() {
                        if op.is("TD") {
                            gs.leading = -y;
                        }
                        let next = Matrix::translate(x, y).then(&tlm);
                        if finite(&next) {
                            tlm = next;
                            tm = tlm;
                        }
                    }
                }
                b"Tm" => {
                    if let Some(m) = Matrix::from_operands(&op.operands).filter(finite) {
                        tlm = m;
                        tm = m;
                    }
                }
                b"T*" => {
                    tlm = Matrix::translate(0.0, -gs.leading).then(&tlm);
                    tm = tlm;
                }
                b"Tj" | b"TJ" | b"'" | b"\"" => {
                    if op.is("'") || op.is("\"") {
                        if op.is("\"") {
                            gs.word_spacing = op.num(0).filter(|v| v.is_finite()).unwrap_or(gs.word_spacing);
                            gs.char_spacing = op.num(1).filter(|v| v.is_finite()).unwrap_or(gs.char_spacing);
                        }
                        tlm = Matrix::translate(0.0, -gs.leading).then(&tlm);
                        tm = tlm;
                    }
                    tm = self.show(&op, &gs, tm);
                }
                b"Do" => {
                    if let Some(name) = op.name(0) {
                        self.xobject(name, resources, &gs, depth);
                    }
                }
                b"BI" => {
                    if let Some((dict, data)) = &op.inline {
                        self.inline_image(dict, data, resources, &gs);
                    }
                }
                b"sh" => {
                    self.unmeasure("shadings");
                }
                _ => {}
            }
        }
    }

    /// Cover the stroke of `path` with one rectangle per segment (square ends).
    fn stroke(&mut self, path: &[Vec<P>], closed: &[bool], gs: &Gs, value: f32) {
        let [a, b, c, d, ..] = gs.ctm.0;
        let scale = (a * d - b * c).abs().sqrt();
        let width = gs.line_width * scale / self.cell;
        let half = if width.is_finite() { width.max(1.0) / 2.0 } else { 0.5 };
        for (i, sp) in path.iter().enumerate() {
            let n = sp.len();
            let segments = if closed.get(i).copied().unwrap_or(false) && n > 2 { n } else { n.saturating_sub(1) };
            if n == 1
                && let Some(&p) = sp.first()
            {
                let square = vec![(p.0 - half, p.1 - half), (p.0 + half, p.1 - half), (p.0 + half, p.1 + half), (p.0 - half, p.1 + half)];
                self.grid.fill(&[square], false, gs.clip, value);
            }
            for s in 0..segments {
                let (Some(&p), Some(&q)) = (sp.get(s), sp.get((s + 1) % n)) else { continue };
                let (dx, dy) = (q.0 - p.0, q.1 - p.1);
                let len = (dx * dx + dy * dy).sqrt();
                let (ux, uy) = if len > 1e-9 && len.is_finite() { (dx / len * half, dy / len * half) } else { (half, 0.0) };
                let (nx, ny) = (-uy, ux);
                let quad = vec![
                    (p.0 - ux + nx, p.1 - uy + ny),
                    (q.0 + ux + nx, q.1 + uy + ny),
                    (q.0 + ux - nx, q.1 + uy - ny),
                    (p.0 - ux - nx, p.1 - uy - ny),
                ];
                self.grid.fill(&[quad], false, gs.clip, value);
                if self.grid.truncated {
                    return;
                }
            }
        }
    }

    /// Paint the box of a text-showing operator and return the advanced text matrix.
    fn show(&mut self, op: &Op, gs: &Gs, tm: Matrix) -> Matrix {
        let font = gs.font.clone().unwrap_or_else(|| Rc::new(Font::fallback()));
        let mut x = 0.0f64;
        let (mut lo, mut hi) = (0.0f64, 0.0f64);
        let mut shown = false;
        let advance = |bytes: &[u8], x: &mut f64| {
            let codes: Vec<(u32, bool)> = if font.two_byte {
                bytes.chunks(2).map(|c| (c.iter().fold(0u32, |a, b| (a << 8) | u32::from(*b)), false)).collect()
            } else {
                bytes.iter().map(|b| (u32::from(*b), *b == 32)).collect()
            };
            for (code, space) in codes {
                let w = font.width(code) * gs.size + gs.char_spacing + if space { gs.word_spacing } else { 0.0 };
                *x += w * gs.scaling;
            }
        };
        let strings: Vec<&Object> = match op.op.as_slice() {
            b"TJ" => op.operands.first().and_then(Object::as_array).map(|a| a.iter().collect()).unwrap_or_default(),
            b"\"" => op.operands.get(2).into_iter().collect(),
            _ => op.operands.first().into_iter().collect(),
        };
        for s in strings {
            match s {
                Object::String(s) => {
                    advance(&s.bytes, &mut x);
                    shown = true;
                }
                other => {
                    if let Some(n) = other.as_f64().filter(|n| n.is_finite()) {
                        x -= n / 1000.0 * gs.size * gs.scaling;
                    }
                }
            }
            if !x.is_finite() {
                x = 0.0;
            }
            lo = lo.min(x);
            hi = hi.max(x);
        }
        let next = Matrix::translate(x, 0.0).then(&tm);
        let next = if finite(&next) { next } else { tm };
        let paints = match gs.render.rem_euclid(8) {
            0 | 4 => (true, false),
            1 | 5 => (false, true),
            2 | 6 => (true, true),
            _ => (false, false),
        };
        if !shown || hi - lo <= 0.0 || (!paints.0 && !paints.1) {
            return next;
        }
        let (y0, y1) = (font.descent * gs.size + gs.rise, font.ascent * gs.size + gs.rise);
        let device = tm.then(&gs.ctm).then(&self.to_grid);
        let quad = vec![device.apply(lo, y0), device.apply(hi, y0), device.apply(hi, y1), device.apply(lo, y1)];
        // Stroked text paints last, so its ink is what the box shows.
        let paint = if paints.1 { &gs.stroke } else { &gs.fill };
        if paints.0 && paints.1 {
            // Count the fill too, so exact/converted totals cover both inks.
            let _ = self.paint_counted(gs.fill.ink(), Self::paint_reason(&gs.fill));
        }
        if let Some(v) = self.paint_counted(paint.ink(), Self::paint_reason(paint)) {
            self.grid.fill(&[quad], false, gs.clip, v);
        }
        next
    }

    fn xobject(&mut self, name: &[u8], resources: &Dict, gs: &Gs, depth: usize) {
        let entry = resources.get(b"XObject").map(|d| self.doc.resolve(d)).and_then(|d| d.as_dict().and_then(|d| d.get(name).cloned()));
        let Some(entry) = entry else { return };
        let reference = entry.as_ref();
        if reference.is_some_and(|r| self.seen.contains(&r)) {
            return;
        }
        let object = self.doc.resolve(&entry);
        let Object::Stream(stream) = object.as_ref() else { return };
        match stream.dict.name(b"Subtype") {
            Some(b"Form") => {
                if depth + 1 >= MAX_DEPTH {
                    self.truncated = true;
                    return;
                }
                if !self.budget_stream() {
                    return;
                }
                let matrix = stream
                    .dict
                    .get(b"Matrix")
                    .map(|m| self.doc.resolve(m))
                    .and_then(|o| o.as_array().and_then(|a| Matrix::from_operands(a)))
                    .unwrap_or_default();
                let mut inner = gs.clone();
                let ctm = matrix.then(&gs.ctm);
                if !finite(&ctm) {
                    return;
                }
                inner.ctm = ctm;
                let bbox = stream.dict.get(b"BBox").map(|b| self.doc.resolve(b)).and_then(|b| {
                    let v: Vec<f64> = b.as_array()?.iter().filter_map(Object::as_f64).collect();
                    match v.as_slice() {
                        [a, b, c, d] if v.iter().all(|x| x.is_finite()) => Some([a.min(*c), b.min(*d), a.max(*c), b.max(*d)]),
                        _ => None,
                    }
                });
                if let Some(bbox) = bbox {
                    inner.clip = intersect(inner.clip, ctm.then(&self.to_grid).bbox(bbox));
                }
                let remaining = MAX_CONTENT.saturating_sub(self.bytes);
                let data = match stream.decoded_within(remaining) {
                    Ok(d) => d,
                    Err(_) => {
                        self.unmeasure("unreadable content streams");
                        return;
                    }
                };
                self.bytes = self.bytes.saturating_add(data.len());
                let own = stream.dict.get(b"Resources").map(|o| self.doc.resolve(o));
                let res = own.as_ref().and_then(|o| o.as_dict()).unwrap_or(resources);
                if let Some(r) = reference {
                    self.seen.insert(r);
                }
                self.run(&data, res, inner, depth + 1);
                if let Some(r) = reference {
                    self.seen.remove(&r);
                }
            }
            Some(b"Image") => {
                let dict = stream.dict.clone();
                self.image(&dict, stream, resources, gs);
            }
            _ => {}
        }
    }

    fn inline_image(&mut self, dict: &Dict, data: &[u8], resources: &Dict, gs: &Gs) {
        let mut full = Dict::new();
        for (k, v) in dict.iter() {
            let key: &[u8] = match k.as_slice() {
                b"W" => b"Width",
                b"H" => b"Height",
                b"BPC" => b"BitsPerComponent",
                b"CS" => b"ColorSpace",
                b"D" => b"Decode",
                b"IM" => b"ImageMask",
                b"F" => b"Filter",
                b"DP" => b"DecodeParms",
                other => other,
            };
            full.set(key.to_vec(), v.clone());
        }
        let stream = Stream::from_raw(full.clone(), data.to_vec());
        self.image(&full, &stream, resources, gs);
    }

    /// Sample an image into the cells it covers.
    fn image(&mut self, dict: &Dict, stream: &Stream, resources: &Dict, gs: &Gs) {
        let int = |k: &[u8]| dict.get(k).and_then(|o| self.doc.resolve(o).as_int());
        let mask = dict.get(b"ImageMask").is_some_and(|o| matches!(self.doc.resolve(o).as_ref(), Object::Bool(true)));
        let (Some(w), Some(h)) = (int(b"Width"), int(b"Height")) else {
            self.unmeasure("unreadable images");
            return;
        };
        let (Ok(w), Ok(h)) = (usize::try_from(w), usize::try_from(h)) else {
            self.unmeasure("unreadable images");
            return;
        };
        let bpc = if mask { 1 } else { int(b"BitsPerComponent").unwrap_or(8) };
        if w == 0 || h == 0 || !matches!(bpc, 1 | 2 | 4 | 8 | 16) {
            self.unmeasure("unreadable images");
            return;
        }
        let bpc = bpc as usize;
        let space = if mask {
            Space::Gray
        } else {
            match dict.get(b"ColorSpace") {
                Some(cs) => self.colour_space(cs, resources, 0),
                None => {
                    self.unmeasure("unreadable images");
                    return;
                }
            }
        };
        let n = space.components();
        if n == 0 || n > MAX_COMPONENTS || matches!(space, Space::Unknown(_) | Space::Pattern) {
            let _ = self.paint_counted(None, "unsupported colour spaces");
            return;
        }
        let Some(row_bytes) = w.checked_mul(n).and_then(|v| v.checked_mul(bpc)).map(|bits| bits.div_ceil(8)) else {
            self.unmeasure("oversized images");
            return;
        };
        let Some(total) = row_bytes.checked_mul(h).filter(|t| *t <= MAX_IMAGE_BYTES) else {
            self.unmeasure("oversized images");
            return;
        };
        let data = match stream.decoded_within(total.saturating_add(4096)) {
            Ok(d) => d,
            Err(_) => {
                self.unmeasure("images in undecoded formats (DCT, JPX, JBIG2, CCITT) or damaged");
                return;
            }
        };
        let max_sample = ((1u32 << bpc) - 1) as f64;
        let decode: Vec<f64> = dict
            .get(b"Decode")
            .map(|o| self.doc.resolve(o))
            .and_then(|o| o.as_array().map(|a| a.iter().filter_map(Object::as_f64).filter(|v| v.is_finite()).collect()))
            .filter(|d: &Vec<f64>| d.len() == 2 * n)
            .unwrap_or_else(|| {
                if matches!(space, Space::Indexed { .. }) {
                    vec![0.0, max_sample]
                } else if matches!(space, Space::Lab) {
                    vec![0.0, 100.0, -100.0, 100.0, -100.0, 100.0]
                } else {
                    (0..n).flat_map(|_| [0.0, 1.0]).collect()
                }
            });
        let ink_of = |comps: &[f64]| space.ink(comps);
        let fill_ink = if mask { gs.fill.ink() } else { None };
        if mask && fill_ink.is_none() {
            let _ = self.paint_counted(None, Self::paint_reason(&gs.fill));
            return;
        }
        let converted = if mask { fill_ink.is_some_and(|i| i.converted) } else { ink_of(&space.initial()).is_some_and(|i| i.converted) };
        let Some(_) = self.paint_counted(Some(Ink { total: 0.0, converted }), "unreadable images") else { return };
        let device = gs.ctm.then(&self.to_grid);
        let Some(inverse) = device.invert() else { return };
        let bbox = intersect(device.bbox([0.0, 0.0, 1.0, 1.0]), gs.clip);
        if !bbox.iter().all(|v| v.is_finite()) {
            return;
        }
        let (x0, x1) = Grid::span(bbox[0], bbox[2], self.grid.w);
        let (y0, y1) = Grid::span(bbox[1], bbox[3], self.grid.h);
        if x0 >= x1 || y0 >= y1 || !self.grid.spend(((x1 - x0) as u64).saturating_mul((y1 - y0) as u64)) {
            return;
        }
        let sample = |px: usize, py: usize, c: usize| -> Option<f64> {
            let bit = py.checked_mul(row_bytes)?.checked_mul(8)?.checked_add((px.checked_mul(n)?.checked_add(c)?).checked_mul(bpc)?)?;
            let byte = *data.get(bit / 8)?;
            let raw = match bpc {
                8 => u32::from(byte),
                16 => (u32::from(byte) << 8) | u32::from(*data.get(bit / 8 + 1)?),
                _ => (u32::from(byte) >> 8usize.checked_sub(bpc + bit % 8)?) & ((1u32 << bpc) - 1),
            };
            let (d0, d1) = (decode.get(2 * c).copied().unwrap_or(0.0), decode.get(2 * c + 1).copied().unwrap_or(1.0));
            Some(d0 + f64::from(raw) * (d1 - d0) / max_sample)
        };
        let mut comps = [0.0f64; MAX_COMPONENTS];
        for row in y0..y1 {
            for col in x0..x1 {
                let (u, v) = inverse.apply(col as f64 + 0.5, row as f64 + 0.5);
                if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {
                    continue;
                }
                let px = ((u * w as f64) as usize).min(w - 1);
                let py = (((1.0 - v) * h as f64) as usize).min(h - 1);
                let value = if mask {
                    // A stencil mask paints the fill colour where the sample decodes to 0.
                    let Some(s) = sample(px, py, 0) else { continue };
                    if s >= 0.5 {
                        continue;
                    }
                    fill_ink.map_or(0.0, |i| i.total)
                } else {
                    let mut ok = true;
                    for (c, slot) in comps.iter_mut().enumerate().take(n) {
                        match sample(px, py, c) {
                            Some(s) => *slot = s,
                            None => ok = false,
                        }
                    }
                    if !ok {
                        continue;
                    }
                    let Some(ink) = comps.get(..n).and_then(ink_of) else { continue };
                    ink.total
                };
                if let Some(cell) = self.grid.cells.get_mut(row * self.grid.w + col) {
                    *cell = if value.is_finite() { value.max(0.0) } else { 0.0 };
                }
            }
        }
    }
}
