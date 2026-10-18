//! Ink separations and total area coverage (TAC): what each printing plate (process C, M, Y,
//! K and every spot or DeviceN colorant) receives at each spot of a page, and the sum of
//! those inks (0–400 %, more with spot inks), with the regions where it exceeds a press limit
//! (Acrobat's Output Preview: the separations list, the ink percentages under the cursor and
//! the "Total Area Coverage" warning; 300 % by default).
//!
//! The page renderer only produces RGB, so this module has its own small, bounded ink
//! rasterizer ([`rasterize`]): it interprets the page's content stream (and the forms it
//! draws) and paints each object's ink into one plate per ink on a grid at a chosen
//! resolution. Later objects knock out earlier ones on every plate, unless overprint is on
//! (`OP`/`op`/`OPM` from `gs`, simulated by default): then a Separation or DeviceN colour
//! paints only its own plates, a process colour only the four process plates, and DeviceCMYK
//! with overprint mode 1 only its non-zero components (ISO 32000-2 §8.6.7).
//!
//! - **Exact** for colours given in DeviceCMYK, DeviceGray (black ink), Separation, DeviceN,
//!   ICCBased with 1 or 4 components, and Indexed over those: their component values *are*
//!   ink percentages. Process colorant names (Cyan, Magenta, Yellow, Black) in Separation and
//!   DeviceN spaces are the process plates; `All` (registration) is every plate; `None` marks
//!   nothing.
//! - **Converted** for DeviceRGB, CalRGB, three-component ICCBased and Lab, through a
//!   [`Conversion`]: the document's output intent profile when the caller supplies one, else
//!   [`BuiltIn`] (no press profile; full black generation with 70 % under-colour removal, the
//!   same approximation as Convert Colors, so RGB black is 190 %). Each page says how many
//!   painted objects were converted and with what.
//! - **Not measured** (counted per reason): pattern and shading fills, images in codecs the
//!   filters don't decode (DCT, JPX, JBIG2, CCITT), oversized images, unknown colour spaces
//!   and annotations. Transparency (soft masks, constant alpha, blend modes) is painted
//!   opaque and counted under `approximated`.
//!
//! Geometry is approximate at the grid's resolution: fills use the real outline (non-zero or
//! even-odd), strokes are covered segment by segment with square ends, text is measured as
//! the box of each shown string (font widths, ascent and descent), images are sampled per
//! cell and clipping paths count as their bounding boxes. A cell is painted when its centre
//! is inside. All work is capped (grid cells across all plates, content bytes, operators,
//! painted objects, rasterization work, tracked inks and nesting); a page that hits a cap
//! says so with `truncated`.

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
/// Grid cells across all plates of a page (2 bytes each): a page with many inks is measured
/// at a lower resolution.
pub const MAX_PLATE_CELLS: usize = 32_000_000;
/// Inks tracked per page (the four process inks included); more are counted as unmeasured.
pub const MAX_INKS: usize = 64;
pub const DEFAULT_REGIONS: usize = 5;
pub const MAX_REGIONS: usize = 50;
/// The process inks, in plate order.
pub const PROCESS_INKS: [&str; 4] = ["Cyan", "Magenta", "Yellow", "Black"];

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
/// Resource entries looked at while collecting a page's inks.
const MAX_DECLARED: usize = 20_000;
/// Plate values are hundredths of a percent.
const FULL: f64 = 10_000.0;

/// How the measurement works and what it can't see, for reports.
pub const METHOD: &str = "Ink coverage is rasterized per plate by PdfCraft's own content interpreter (the page renderer is RGB-only). \
CMYK, Gray (as black), Separation, DeviceN and Indexed colours are exact ink values; RGB and Lab colours are converted to CMYK \
through the document's output intent profile when it has a usable CMYK one, else a built-in approximation without a press \
profile (full black generation, 70% under-colour removal; RGB black is 190%). Overprint (OP/op/OPM) is simulated; transparency \
is painted opaque. Patterns, shadings, DCT/JPX/JBIG2/CCITT images and annotations are not measured; clipping paths count as \
their bounding boxes and text as string boxes.";

/// The built-in conversion's description, for reports.
pub const BUILT_IN: &str = "built-in RGB to CMYK without a press profile: full black generation with 70% under-colour removal \
(RGB black becomes 30/30/30/100, 190% total); Lab and CalRGB are read through sRGB";

/// How RGB and Lab colours become process inks (0–1 each).
pub trait Conversion {
    /// What converts, for reports.
    fn describe(&self) -> String;
    /// Whether an ICC output profile does the conversion.
    fn uses_profile(&self) -> bool {
        false
    }
    /// CMYK for an sRGB colour (components 0–1).
    fn rgb(&self, rgb: [f64; 3]) -> [f64; 4];
    /// CMYK for a CIE L*a*b* colour (D50; L* 0–100).
    fn lab(&self, lab: [f64; 3]) -> [f64; 4];
}

/// The press-agnostic conversion: full black generation with 70 % under-colour removal.
#[derive(Clone, Copy, Debug, Default)]
pub struct BuiltIn;

impl Conversion for BuiltIn {
    fn describe(&self) -> String {
        BUILT_IN.into()
    }
    fn rgb(&self, rgb: [f64; 3]) -> [f64; 4] {
        rgb_to_cmyk(rgb)
    }
    fn lab(&self, lab: [f64; 3]) -> [f64; 4] {
        rgb_to_cmyk(lab_to_srgb(lab))
    }
}

/// RGB → CMYK without a press profile (as Convert Colors' built-in conversion).
pub fn rgb_to_cmyk([r, g, b]: [f64; 3]) -> [f64; 4] {
    let (c, m, y) = (1.0 - clamp01(r), 1.0 - clamp01(g), 1.0 - clamp01(b));
    let k = c.min(m).min(y);
    let ucr = 0.7 * k;
    [clamp01(c - ucr), clamp01(m - ucr), clamp01(y - ucr), k]
}

/// D50 L*a*b* → sRGB (0–1), through the D50-adapted sRGB matrix.
pub fn lab_to_srgb([l, a, b]: [f64; 3]) -> [f64; 3] {
    let fy = (l + 16.0) / 116.0;
    let (fx, fz) = (fy + a / 500.0, fy - b / 200.0);
    let g = |t: f64| if t > 6.0 / 29.0 { t * t * t } else { 108.0 / 841.0 * (t - 4.0 / 29.0) };
    let (x, y, z) = (g(fx) * 0.9642, g(fy), g(fz) * 0.8249);
    let lin = [
        3.1338561 * x - 1.6168667 * y - 0.4906146 * z,
        -0.9787684 * x + 1.9161415 * y + 0.0334540 * z,
        0.0719453 * x - 0.2289914 * y + 1.4052427 * z,
    ];
    lin.map(|v| {
        let v = clamp01(v);
        clamp01(if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 })
    })
}

/// What to measure.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// The press limit in percent: cells above it are reported.
    pub limit: f64,
    /// Requested grid resolution (cells per inch).
    pub dpi: f64,
    /// How many of the worst regions to return per page.
    pub max_regions: usize,
    /// Simulate overprint (`OP`, `op`, `OPM`); off, every object knocks out what is below.
    pub overprint: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { limit: DEFAULT_LIMIT, dpi: DEFAULT_DPI, max_regions: DEFAULT_REGIONS, overprint: true }
    }
}

fn check_dpi(dpi: f64) -> Result<(), String> {
    if !dpi.is_finite() || !(MIN_DPI..=MAX_DPI).contains(&dpi) {
        return Err(format!("dpi must be between {MIN_DPI} and {MAX_DPI}"));
    }
    Ok(())
}

impl Options {
    pub fn validate(&self) -> Result<(), String> {
        if !self.limit.is_finite() || !(MIN_LIMIT..=MAX_LIMIT).contains(&self.limit) {
            return Err(format!("limit must be a percentage between {MIN_LIMIT} and {MAX_LIMIT}"));
        }
        check_dpi(self.dpi)?;
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
    /// Where it is: the centre of the topmost, then leftmost, cell with the highest total, in
    /// user space (`None` on a page without ink).
    pub max_at: Option<[f64; 2]>,
    /// Area above the limit, in square points.
    pub over_area: f64,
    /// That area as a percentage of the page area.
    pub over_percent: f64,
    /// The worst regions (highest coverage first, then largest), at most `max_regions`.
    pub regions: Vec<Region>,
    /// How many separate regions exceed the limit.
    pub region_count: usize,
    /// The resolution used (lower than requested when the page would exceed the cell budget).
    pub dpi: f64,
    /// Painted objects whose ink is exact (CMYK, Gray, Separation, DeviceN, Indexed of those).
    pub exact_paints: usize,
    /// Painted objects whose ink came from the RGB/Lab → CMYK conversion.
    pub converted_paints: usize,
    /// The conversion used for them.
    pub conversion: String,
    /// Objects that could not be measured, by reason.
    pub unmeasured: BTreeMap<&'static str, usize>,
    /// Objects measured with a simplification (transparency painted opaque), by reason.
    pub approximated: BTreeMap<&'static str, usize>,
    /// A cap was reached; part of the page was not measured.
    pub truncated: bool,
}

impl PageCoverage {
    /// Whether any part of the page is over the limit.
    pub fn flagged(&self) -> bool {
        self.region_count > 0
    }
}

/// Measure 0-based page `index` with the built-in RGB conversion.
pub fn page_coverage(doc: &Document, index: usize, options: &Options) -> Result<PageCoverage, String> {
    page_coverage_with(doc, index, options, &BuiltIn)
}

/// Measure 0-based page `index`, converting RGB and Lab colours with `conversion`.
pub fn page_coverage_with(doc: &Document, index: usize, options: &Options, conversion: &dyn Conversion) -> Result<PageCoverage, String> {
    options.validate()?;
    let raster = rasterize(doc, index, options.dpi, options.overprint, conversion)?;
    let totals = raster.totals();
    let (mut max, mut max_cell) = (0.0f32, None);
    // Rows run bottom-up, so scan from the top row for the topmost, leftmost maximum.
    for row in (0..raster.height).rev() {
        for col in 0..raster.width {
            let v = totals.get(row * raster.width + col).copied().unwrap_or(0.0);
            if v > max {
                max = v;
                max_cell = Some((col, row));
            }
        }
    }
    let (regions, region_count, over_cells) = regions(&totals, raster.width, raster.height, options.limit as f32, options.max_regions);
    let (crop, cell) = (raster.crop, raster.cell());
    let cell_area = cell * cell;
    let over_area = over_cells as f64 * cell_area;
    let page_area = (crop[2] - crop[0]) * (crop[3] - crop[1]);
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
        max_at: max_cell.map(|(c, r)| raster.centre(c, r)),
        over_area: round(over_area),
        over_percent: round((over_area / page_area * 100.0).min(100.0)),
        regions: regions
            .into_iter()
            .map(|(bbox, max, cells)| Region { bbox: to_user(bbox), max: round(f64::from(max)), area: round(cells as f64 * cell_area) })
            .collect(),
        region_count,
        dpi: round(raster.dpi),
        exact_paints: raster.exact_paints,
        converted_paints: raster.converted_paints,
        conversion: raster.conversion.clone(),
        unmeasured: raster.unmeasured.clone(),
        approximated: raster.approximated.clone(),
        truncated: raster.truncated,
    })
}

fn round(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

// ---- separations ----------------------------------------------------------------------------

/// One ink (printing plate).
#[derive(Clone, Debug, PartialEq)]
pub struct InkInfo {
    /// The colorant name (`Cyan` … `Black` for the process inks).
    pub name: String,
    /// A process ink (C, M, Y or K), else a spot colorant.
    pub process: bool,
    /// Where the spot colorant was defined: `Separation` or `DeviceN` (`None` for process inks).
    pub defined_by: Option<&'static str>,
    /// The alternate colour space's family (`DeviceCMYK`, `ICCBased (4)` …).
    pub alternate: Option<String>,
    /// The tint transform's function type (0, 2, 3 or 4).
    pub tint_transform: Option<i64>,
    /// The alternate's components at 100 % tint, when the tint transform is a Type 2 or a
    /// one-input Type 0 function into a gray, RGB or CMYK alternate.
    pub alternate_value: Option<Vec<f64>>,
    /// An sRGB colour to preview the ink with (0–1): the process colour, the alternate at
    /// 100 % tint, or mid gray when that can't be evaluated.
    pub preview: [f64; 3],
    /// Whether `preview` comes from the ink itself (process or evaluated alternate).
    pub preview_exact: bool,
}

impl InkInfo {
    pub(crate) fn process(i: usize) -> Self {
        let preview = match i {
            0 => [0.0, 1.0, 1.0],
            1 => [1.0, 0.0, 1.0],
            2 => [1.0, 1.0, 0.0],
            _ => [0.0, 0.0, 0.0],
        };
        Self {
            name: PROCESS_INKS.get(i).copied().unwrap_or("Black").into(),
            process: true,
            defined_by: None,
            alternate: None,
            tint_transform: None,
            alternate_value: None,
            preview,
            preview_exact: true,
        }
    }
}

/// A page separated into plates, one per ink.
#[derive(Clone, Debug)]
pub struct Raster {
    /// 0-based page index.
    pub page: usize,
    /// The measured area (the crop box) in user space.
    pub crop: [f64; 4],
    /// The resolution used, in cells per inch.
    pub dpi: f64,
    /// Grid size in cells; row 0 is the bottom of the crop box.
    pub width: usize,
    pub height: usize,
    /// The page's inks: the four process inks, then spot colorants in the order met.
    pub inks: Vec<InkInfo>,
    /// Whether each ink puts any ink on the page (spot inks can be declared but unused).
    pub painted: Vec<bool>,
    plates: Vec<Vec<u16>>,
    pub exact_paints: usize,
    pub converted_paints: usize,
    pub unmeasured: BTreeMap<&'static str, usize>,
    pub approximated: BTreeMap<&'static str, usize>,
    pub truncated: bool,
    /// Overprint was simulated.
    pub overprint: bool,
    /// How RGB and Lab colours were converted.
    pub conversion: String,
    pub conversion_from_profile: bool,
}

impl Raster {
    /// The size of a cell, in points.
    pub fn cell(&self) -> f64 {
        72.0 / self.dpi
    }

    /// The ink named `name` (exact colorant name).
    pub fn ink(&self, name: &str) -> Option<usize> {
        self.inks.iter().position(|i| i.name == name)
    }

    /// Ink `ink` at cell (`col`, `row`), in percent.
    pub fn value(&self, ink: usize, col: usize, row: usize) -> f64 {
        if col >= self.width || row >= self.height {
            return 0.0;
        }
        let at = row.saturating_mul(self.width).saturating_add(col);
        self.plates.get(ink).and_then(|p| p.get(at)).map_or(0.0, |v| f64::from(*v) / 100.0)
    }

    /// The sum of every ink at cell (`col`, `row`), in percent.
    pub fn total(&self, col: usize, row: usize) -> f64 {
        (0..self.plates.len()).map(|i| self.value(i, col, row)).sum()
    }

    /// The cell holding the user-space point (`x`, `y`), when it is inside the crop box.
    pub fn cell_of(&self, x: f64, y: f64) -> Option<(usize, usize)> {
        let [x0, y0, x1, y1] = self.crop;
        if !(x.is_finite() && y.is_finite()) || x < x0 || x > x1 || y < y0 || y > y1 || self.width == 0 || self.height == 0 {
            return None;
        }
        let cell = self.cell();
        let col = (((x - x0) / cell).floor().max(0.0) as usize).min(self.width - 1);
        let row = (((y - y0) / cell).floor().max(0.0) as usize).min(self.height - 1);
        Some((col, row))
    }

    /// The user-space centre of cell (`col`, `row`).
    pub fn centre(&self, col: usize, row: usize) -> [f64; 2] {
        let cell = self.cell();
        let x = (self.crop[0] + (col as f64 + 0.5) * cell).min(self.crop[2]);
        let y = (self.crop[1] + (row as f64 + 0.5) * cell).min(self.crop[3]);
        [round(x), round(y)]
    }

    /// The total ink of every cell, in percent (row 0 at the bottom).
    pub fn totals(&self) -> Vec<f32> {
        let cells = self.width.saturating_mul(self.height);
        let mut out = vec![0.0f32; cells];
        for plate in self.plates.iter().filter(|p| p.len() == cells) {
            for (t, v) in out.iter_mut().zip(plate) {
                *t += f32::from(*v) / 100.0;
            }
        }
        out
    }

    /// Plate `ink` as an 8-bit grayscale image, top row first, the way a film separation
    /// looks: 0 (black) is 100 % ink, 255 (white) none.
    pub fn plate_gray(&self, ink: usize) -> Option<Vec<u8>> {
        if ink >= self.inks.len() {
            return None;
        }
        let mut out = Vec::with_capacity(self.width.saturating_mul(self.height));
        for row in (0..self.height).rev() {
            for col in 0..self.width {
                let v = self.value(ink, col, row) / 100.0;
                out.push((255.0 - (v.clamp(0.0, 1.0) * 255.0)).round() as u8);
            }
        }
        Some(out)
    }

    /// A colour preview of the inks in `shown` (others hidden), as 8-bit RGB, top row first:
    /// each ink darkens the paper towards its preview colour by its percentage (multiply).
    pub fn composite(&self, shown: &[usize]) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.width.saturating_mul(self.height).saturating_mul(3));
        for row in (0..self.height).rev() {
            for col in 0..self.width {
                let mut rgb = [1.0f64; 3];
                for &ink in shown {
                    let Some(info) = self.inks.get(ink) else { continue };
                    let t = (self.value(ink, col, row) / 100.0).clamp(0.0, 1.0);
                    for (c, p) in rgb.iter_mut().zip(info.preview) {
                        *c *= 1.0 - t * (1.0 - clamp01(p));
                    }
                }
                out.extend(rgb.map(|c| (clamp01(c) * 255.0).round() as u8));
            }
        }
        out
    }
}

/// Separate 0-based page `index` into plates at `dpi` (lowered when the page, times its inks,
/// would exceed the cell budget), simulating overprint when `overprint` is set and converting
/// RGB and Lab colours with `conversion`.
pub fn rasterize(doc: &Document, index: usize, dpi: f64, overprint: bool, conversion: &dyn Conversion) -> Result<Raster, String> {
    check_dpi(dpi)?;
    let pages = pdfcraft_model::pages(doc);
    let page = pages.get(index).ok_or_else(|| format!("page {} does not exist; the document has {} pages", index.saturating_add(1), pages.len()))?;
    let crop = page.crop(doc);
    let (width, height) = (crop[2] - crop[0], crop[3] - crop[1]);
    if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0) {
        return Err("the page has an empty or invalid crop box".into());
    }
    let mut walker = Walker::new(doc, conversion, overprint);
    let resources = page.dict.get(b"Resources").map(|o| doc.resolve(o));
    let empty = Dict::new();
    let resources = resources.as_ref().and_then(|o| o.as_dict()).unwrap_or(&empty);
    // Collect the page's inks first: the cell budget is shared by their plates.
    walker.declare(resources, 0);
    let budget = (MAX_PLATE_CELLS / walker.grid.plates.len().max(PROCESS_INKS.len())).clamp(1, MAX_CELLS);
    // Choose the resolution: requested, lowered until the grid fits the cell budget.
    let mut dpi = dpi;
    let cells_at = |dpi: f64| ((width * dpi / 72.0).ceil().max(1.0), (height * dpi / 72.0).ceil().max(1.0));
    let (w, h) = cells_at(dpi);
    if w * h > budget as f64 {
        dpi *= (budget as f64 / (w * h)).sqrt() * 0.999;
    }
    let (mut w, mut h) = cells_at(dpi);
    // Extremely thin pages: keep each side within the budget too.
    w = w.min(budget as f64);
    h = h.min((budget as f64 / w).floor().max(1.0));
    let (gw, gh) = (w as usize, h as usize);
    let cell = 72.0 / dpi;
    walker.grid.w = gw;
    walker.grid.h = gh;
    walker.cell = cell;
    walker.to_grid = Matrix([1.0 / cell, 0.0, 0.0, 1.0 / cell, -crop[0] / cell, -crop[1] / cell]);
    let gs = Gs::new([0.0, 0.0, gw as f64, gh as f64]);
    if let Some(contents) = page.dict.get(b"Contents") {
        let data = walker.content(contents);
        walker.run(&data, resources, gs, 0);
    }
    if page.dict.get(b"Annots").map(|o| doc.resolve(o)).and_then(|o| o.as_array().map(Vec::len)).is_some_and(|n| n > 0) {
        walker.unmeasure("annotations");
    }
    let truncated = walker.truncated || walker.grid.truncated;
    if walker.grid.over_budget {
        walker.unmeasure("inks beyond the plate memory budget");
    }
    Ok(Raster {
        page: index,
        crop,
        dpi,
        width: gw,
        height: gh,
        inks: walker.inks,
        painted: walker.grid.painted,
        plates: walker.grid.plates,
        exact_paints: walker.exact,
        converted_paints: walker.converted,
        unmeasured: walker.unmeasured,
        approximated: walker.approximated,
        truncated,
        overprint,
        conversion: conversion.describe(),
        conversion_from_profile: conversion.uses_profile(),
    })
}

/// Connected (8-neighbour) regions of cells above `limit`: the worst `keep`, the number of
/// regions and the number of cells above the limit.
/// A found region: cell bounds, highest value, cell count.
type Found = ([usize; 4], f32, usize);

fn regions(cells: &[f32], w: usize, h: usize, limit: f32, keep: usize) -> (Vec<Found>, usize, usize) {
    let mut visited = vec![false; cells.len()];
    let mut out: Vec<Found> = Vec::new();
    let (mut count, mut over) = (0usize, 0usize);
    let mut stack = Vec::new();
    let order = |a: &Found, b: &Found| b.1.total_cmp(&a.1).then(b.2.cmp(&a.2));
    for start in 0..cells.len() {
        if cells.get(start).is_none_or(|v| *v <= limit) || visited.get(start).copied().unwrap_or(true) {
            continue;
        }
        count += 1;
        let (mut bbox, mut max, mut count_cells) = ([usize::MAX, usize::MAX, 0, 0], 0.0f32, 0usize);
        if let Some(v) = visited.get_mut(start) {
            *v = true;
        }
        stack.push(start);
        while let Some(at) = stack.pop() {
            let (x, y) = (at % w, at / w);
            let value = cells.get(at).copied().unwrap_or(0.0);
            count_cells += 1;
            max = max.max(value);
            bbox = [bbox[0].min(x), bbox[1].min(y), bbox[2].max(x + 1), bbox[3].max(y + 1)];
            for (dx, dy) in [(-1i64, -1i64), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
                let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                    continue;
                }
                let next = ny as usize * w + nx as usize;
                if cells.get(next).is_some_and(|v| *v > limit)
                    && let Some(seen) = visited.get_mut(next)
                    && !*seen
                {
                    *seen = true;
                    stack.push(next);
                }
            }
        }
        over += count_cells;
        out.push((bbox, max, count_cells));
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

/// Which plate a colorant puts its ink on.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Target {
    /// `None`: marks nothing.
    Nothing,
    /// `All`: every plate (registration).
    All,
    Plate(usize),
    /// A colorant beyond [`MAX_INKS`].
    Untracked,
}

/// Which plates an overprinting object paints.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Scope {
    /// The four process plates (all of them, or with overprint mode 1 in DeviceCMYK only the
    /// non-zero components).
    Process { device_cmyk: bool },
    /// Only the named colorants (Separation, DeviceN).
    Listed,
}

/// How an object's ink was found.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Kind {
    converted: bool,
    scope: Scope,
}

#[derive(Clone, Debug)]
enum Space {
    Gray,
    Rgb,
    /// `device`: DeviceCMYK itself (overprint mode 1 applies), not a four-component ICC space.
    Cmyk {
        device: bool,
    },
    Lab,
    Separation(Target),
    DeviceN(Vec<Target>),
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

const PROCESS: Scope = Scope::Process { device_cmyk: false };

impl Space {
    fn components(&self) -> usize {
        match self {
            Space::Gray | Space::Separation(_) | Space::Indexed { .. } => 1,
            Space::Rgb | Space::Lab => 3,
            Space::Cmyk { .. } => 4,
            Space::DeviceN(t) => t.len(),
            Space::Pattern => 0,
            Space::Unknown(n) => *n,
        }
    }

    fn initial(&self) -> Vec<f64> {
        match self {
            Space::Gray | Space::Rgb | Space::Indexed { .. } => vec![0.0; self.components()],
            Space::Lab => vec![0.0, 0.0, 0.0],
            Space::Cmyk { .. } => vec![0.0, 0.0, 0.0, 1.0],
            Space::Separation(_) | Space::DeviceN(_) => vec![1.0; self.components()],
            Space::Pattern | Space::Unknown(_) => Vec::new(),
        }
    }

    /// The colorants of colour `c` (each with its tint, 0–1) into `out`, or `None` when this
    /// space can't be measured.
    fn ink_into(&self, c: &[f64], conversion: &dyn Conversion, out: &mut Vec<(Target, f64)>) -> Option<Kind> {
        out.clear();
        let at = |i: usize| c.get(i).copied().unwrap_or(0.0);
        let process = |out: &mut Vec<(Target, f64)>, cmyk: [f64; 4]| {
            out.extend(cmyk.iter().enumerate().map(|(i, v)| (Target::Plate(i), clamp01(*v))));
        };
        let exact = Kind { converted: false, scope: PROCESS };
        match self {
            Space::Gray => {
                process(out, [0.0, 0.0, 0.0, 1.0 - clamp01(at(0))]);
                Some(exact)
            }
            Space::Cmyk { device } => {
                process(out, [at(0), at(1), at(2), at(3)]);
                Some(Kind { converted: false, scope: Scope::Process { device_cmyk: *device } })
            }
            Space::Rgb => {
                process(out, conversion.rgb([clamp01(at(0)), clamp01(at(1)), clamp01(at(2))]));
                Some(Kind { converted: true, scope: PROCESS })
            }
            Space::Lab => {
                let l = if at(0).is_finite() { at(0).clamp(0.0, 100.0) } else { 0.0 };
                let ab = |v: f64| if v.is_finite() { v.clamp(-128.0, 128.0) } else { 0.0 };
                process(out, conversion.lab([l, ab(at(1)), ab(at(2))]));
                Some(Kind { converted: true, scope: PROCESS })
            }
            Space::Separation(t) => {
                out.push((*t, clamp01(at(0))));
                Some(Kind { converted: false, scope: Scope::Listed })
            }
            Space::DeviceN(targets) => {
                out.extend(targets.iter().enumerate().map(|(i, t)| (*t, clamp01(at(i)))));
                Some(Kind { converted: false, scope: Scope::Listed })
            }
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
                    comps[1] = comps[1] * 200.0 - 100.0;
                    comps[2] = comps[2] * 200.0 - 100.0;
                }
                base.ink_into(comps.get(..n)?, conversion, out)
            }
            Space::Pattern | Space::Unknown(_) => None,
        }
    }

    fn untracked(&self) -> bool {
        match self {
            Space::Separation(t) => *t == Target::Untracked,
            Space::DeviceN(t) => t.contains(&Target::Untracked),
            Space::Indexed { base, .. } => base.untracked(),
            _ => false,
        }
    }
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
    /// One plate per ink, `w × h` values in hundredths of a percent; empty until inked.
    plates: Vec<Vec<u16>>,
    painted: Vec<bool>,
    allocated: usize,
    over_budget: bool,
    work: u64,
    truncated: bool,
    crossings: Vec<(f64, i32)>,
    /// Per-plate tint while building writes (NaN: not named).
    scratch: Vec<f64>,
}

impl Grid {
    fn new() -> Self {
        Self {
            w: 0,
            h: 0,
            plates: vec![Vec::new(); PROCESS_INKS.len()],
            painted: vec![false; PROCESS_INKS.len()],
            allocated: 0,
            over_budget: false,
            work: 0,
            truncated: false,
            crossings: Vec::new(),
            scratch: Vec::new(),
        }
    }

    fn spend(&mut self, cost: u64) -> bool {
        self.work = self.work.saturating_add(cost);
        if self.work > MAX_WORK {
            self.truncated = true;
            false
        } else {
            true
        }
    }

    fn cells(&self) -> usize {
        self.w.saturating_mul(self.h)
    }

    /// Give plate `p` its cells, within the budget shared by all plates.
    fn allocate(&mut self, p: usize) -> bool {
        let cells = self.cells();
        if self.plates.get(p).is_some_and(|plate| plate.len() == cells) {
            return true;
        }
        if cells == 0 || self.allocated.saturating_add(1).saturating_mul(cells) > MAX_PLATE_CELLS.saturating_add(MAX_PLATE_CELLS / 2) {
            self.over_budget = true;
            return false;
        }
        let Some(plate) = self.plates.get_mut(p) else { return false };
        *plate = vec![0; cells];
        self.allocated = self.allocated.saturating_add(1);
        true
    }

    /// The plate writes (plate, value) for an object whose colorants are `values`: with
    /// overprint off every plate (unnamed ones become 0), with it on only the plates `scope`
    /// covers. Empty when the colour marks nothing (`None` colorants).
    fn build(&mut self, values: &[(Target, f64)], scope: Scope, overprint: bool, opm: i64, out: &mut Vec<(usize, u16)>) {
        out.clear();
        let n = self.plates.len();
        let mut scratch = std::mem::take(&mut self.scratch);
        scratch.clear();
        scratch.resize(n, f64::NAN);
        let mut marks = false;
        let name = |s: &mut f64, v: f64| *s = if s.is_nan() { v } else { s.max(v) };
        for &(t, v) in values {
            match t {
                Target::Nothing | Target::Untracked => {}
                Target::All => {
                    marks = true;
                    for s in scratch.iter_mut() {
                        name(s, v);
                    }
                }
                Target::Plate(p) => {
                    marks = true;
                    if let Some(s) = scratch.get_mut(p) {
                        name(s, v);
                    }
                }
            }
        }
        if marks {
            for (p, s) in scratch.iter().enumerate() {
                let named = !s.is_nan();
                let touched = !overprint
                    || match scope {
                        Scope::Listed => named,
                        Scope::Process { device_cmyk } => p < PROCESS_INKS.len() && (!(device_cmyk && opm == 1) || (named && *s > 0.0)),
                    };
                if !touched {
                    continue;
                }
                let q = if named { (clamp01(*s) * FULL).round() as u16 } else { 0 };
                let have = self.plates.get(p).is_some_and(|plate| !plate.is_empty());
                if q == 0 && !have {
                    continue;
                }
                if q > 0 && !have && !self.allocate(p) {
                    continue;
                }
                if q > 0
                    && let Some(painted) = self.painted.get_mut(p)
                {
                    *painted = true;
                }
                out.push((p, q));
            }
        }
        self.scratch = scratch;
    }

    /// Write one cell.
    fn put(&mut self, at: usize, writes: &[(usize, u16)]) {
        for &(p, v) in writes {
            if let Some(cell) = self.plates.get_mut(p).and_then(|plate| plate.get_mut(at)) {
                *cell = v;
            }
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
    fn fill(&mut self, subpaths: &[Vec<P>], even_odd: bool, clip: [f64; 4], writes: &[(usize, u16)]) {
        if writes.is_empty() {
            return;
        }
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
        if !self.spend(((y1 - y0) as u64).saturating_mul(edges.saturating_add(writes.len() as u64))) {
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
                        self.work = self.work.saturating_add(((b - a) as u64).saturating_mul(writes.len() as u64));
                        let base = row * self.w;
                        for &(p, v) in writes {
                            if let Some(cells) = self.plates.get_mut(p).and_then(|plate| plate.get_mut(base + a..base + b)) {
                                cells.fill(v);
                            }
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
    /// Overprint for fills (`op`) and strokes (`OP`), and the overprint mode.
    op_fill: bool,
    op_stroke: bool,
    opm: i64,
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
            op_fill: false,
            op_stroke: false,
            opm: 0,
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
    conversion: &'a dyn Conversion,
    overprint: bool,
    grid: Grid,
    inks: Vec<InkInfo>,
    ink_index: HashMap<Vec<u8>, usize>,
    to_grid: Matrix,
    cell: f64,
    seen: HashSet<ObjRef>,
    declared: HashSet<ObjRef>,
    declared_entries: usize,
    bytes: usize,
    streams: usize,
    operators: usize,
    paints: usize,
    exact: usize,
    converted: usize,
    unmeasured: BTreeMap<&'static str, usize>,
    approximated: BTreeMap<&'static str, usize>,
    truncated: bool,
    fonts: HashMap<ObjRef, Rc<Font>>,
    values: Vec<(Target, f64)>,
}

fn intersect(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])]
}

fn finite(m: &Matrix) -> bool {
    m.0.iter().all(|v| v.is_finite() && v.abs() < 1e12)
}

/// A colour space family's name and component count, for describing alternates.
fn family(doc: &Document, o: &Object) -> (String, usize) {
    let o = doc.resolve(o);
    let (name, arg) = match o.as_ref() {
        Object::Name(n) => (n.clone(), None),
        Object::Array(a) => (a.first().and_then(|f| doc.resolve(f).as_name().map(<[u8]>::to_vec)).unwrap_or_default(), a.get(1).cloned()),
        _ => (Vec::new(), None),
    };
    let text = String::from_utf8_lossy(&name).into_owned();
    match name.as_slice() {
        b"DeviceGray" | b"CalGray" | b"G" => (text, 1),
        b"DeviceRGB" | b"CalRGB" | b"RGB" | b"Lab" => (text, 3),
        b"DeviceCMYK" | b"CMYK" => (text, 4),
        b"ICCBased" => {
            let n = arg.map(|s| doc.resolve(&s)).and_then(|s| match s.as_ref() {
                Object::Stream(s) => s.dict.get(b"N").and_then(|n| doc.resolve(n).as_int()),
                _ => None,
            });
            let n = n.and_then(|n| usize::try_from(n).ok()).filter(|n| (1..=MAX_COMPONENTS).contains(n)).unwrap_or(0);
            (format!("ICCBased ({n})"), n)
        }
        _ if text.is_empty() => ("unknown".into(), 0),
        _ => (text, 0),
    }
}

/// An sRGB preview of alternate components: gray, RGB or CMYK.
fn preview_of(values: &[f64]) -> Option<[f64; 3]> {
    match *values {
        [g] => Some([clamp01(g); 3]),
        [r, g, b] => Some([clamp01(r), clamp01(g), clamp01(b)]),
        [c, m, y, k] => {
            let k = clamp01(k);
            Some([(1.0 - clamp01(c)) * (1.0 - k), (1.0 - clamp01(m)) * (1.0 - k), (1.0 - clamp01(y)) * (1.0 - k)])
        }
        _ => None,
    }
}

impl<'a> Walker<'a> {
    fn new(doc: &'a Document, conversion: &'a dyn Conversion, overprint: bool) -> Self {
        Self {
            doc,
            conversion,
            overprint,
            grid: Grid::new(),
            inks: (0..PROCESS_INKS.len()).map(InkInfo::process).collect(),
            ink_index: HashMap::new(),
            to_grid: Matrix::IDENTITY,
            cell: 1.0,
            seen: HashSet::new(),
            declared: HashSet::new(),
            declared_entries: 0,
            bytes: 0,
            streams: 0,
            operators: 0,
            paints: 0,
            exact: 0,
            converted: 0,
            unmeasured: BTreeMap::new(),
            approximated: BTreeMap::new(),
            truncated: false,
            fonts: HashMap::new(),
            values: Vec::new(),
        }
    }
}

impl Walker<'_> {
    fn unmeasure(&mut self, why: &'static str) {
        let n = self.unmeasured.entry(why).or_insert(0);
        *n = n.saturating_add(1);
    }

    fn approximate(&mut self, why: &'static str) {
        let n = self.approximated.entry(why).or_insert(0);
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

    /// Count a painted object and return its plate writes, or `None` when it can't be measured
    /// (counted by reason) or a cap was reached.
    fn paint(&mut self, paint: &Paint, overprint: bool, opm: i64) -> Option<Vec<(usize, u16)>> {
        self.paints = self.paints.saturating_add(1);
        if self.paints > MAX_PAINTS {
            self.truncated = true;
            return None;
        }
        if paint.pattern {
            self.unmeasure(Self::paint_reason(paint));
            return None;
        }
        let mut values = std::mem::take(&mut self.values);
        let kind = paint.space.ink_into(&paint.comps, self.conversion, &mut values);
        let out = match kind {
            Some(kind) => {
                if kind.converted {
                    self.converted = self.converted.saturating_add(1);
                } else {
                    self.exact = self.exact.saturating_add(1);
                }
                if paint.space.untracked() {
                    self.unmeasure("inks beyond the tracked limit");
                }
                let mut writes = Vec::new();
                self.grid.build(&values, kind.scope, overprint && self.overprint, opm, &mut writes);
                Some(writes)
            }
            None => {
                self.unmeasure(Self::paint_reason(paint));
                None
            }
        };
        self.values = values;
        out
    }

    fn paint_reason(paint: &Paint) -> &'static str {
        if paint.pattern || matches!(*paint.space, Space::Pattern) { "pattern fills" } else { "unsupported colour spaces" }
    }

    fn device(&self, gs: &Gs) -> Matrix {
        gs.ctm.then(&self.to_grid)
    }

    /// The plate of colorant `name`, registering a new spot ink (described by `describe`).
    fn target(&mut self, name: &[u8], describe: impl FnOnce(&mut Self) -> InkInfo) -> Target {
        match name {
            b"None" => return Target::Nothing,
            b"All" => return Target::All,
            _ => {}
        }
        if let Some(i) = PROCESS_INKS.iter().position(|p| p.as_bytes() == name) {
            return Target::Plate(i);
        }
        if let Some(&i) = self.ink_index.get(name) {
            return Target::Plate(i);
        }
        if self.inks.len() >= MAX_INKS {
            return Target::Untracked;
        }
        let mut info = describe(self);
        info.name = String::from_utf8_lossy(name).into_owned();
        let i = self.inks.len();
        self.inks.push(info);
        self.ink_index.insert(name.to_vec(), i);
        self.grid.plates.push(Vec::new());
        self.grid.painted.push(false);
        Target::Plate(i)
    }

    /// Describe a spot colorant from its alternate space and tint transform (a Separation's,
    /// or a DeviceN's when `defined_by` is DeviceN; its tint transform takes all colorants, so
    /// it is only evaluated for a Separation).
    fn spot(&self, defined_by: &'static str, alternate: Option<&Object>, function: Option<&Object>) -> InkInfo {
        let (alt_name, outputs) = alternate.map_or(("unknown".into(), 0), |a| family(self.doc, a));
        let function_type = function.and_then(|f| {
            let f = self.doc.resolve(f);
            f.as_dict().and_then(|d| d.get(b"FunctionType")).and_then(|t| self.doc.resolve(t).as_int())
        });
        let lab = alt_name == "Lab";
        let value = match (function, defined_by) {
            (Some(f), "Separation") if matches!(outputs, 1 | 3 | 4) && !lab => {
                crate::colour_warnings::parse_tint(self.doc, f, outputs).map(|t| t.eval(1.0)).filter(|v| v.len() == outputs)
            }
            _ => None,
        };
        let preview = value.as_deref().and_then(preview_of);
        InkInfo {
            name: String::new(),
            process: false,
            defined_by: Some(defined_by),
            alternate: Some(alt_name),
            tint_transform: function_type,
            alternate_value: value.map(|v| v.into_iter().map(round_tint).collect()),
            preview: preview.unwrap_or([0.5, 0.5, 0.5]),
            preview_exact: preview.is_some(),
        }
    }

    fn colour_space(&mut self, o: &Object, resources: &Dict, depth: usize) -> Space {
        if depth > 4 {
            return Space::Unknown(1);
        }
        let doc = self.doc;
        let o = doc.resolve(o);
        match o.as_ref() {
            Object::Name(n) => match n.as_slice() {
                b"DeviceGray" | b"G" | b"CalGray" => Space::Gray,
                b"DeviceRGB" | b"RGB" | b"CalRGB" => Space::Rgb,
                b"DeviceCMYK" | b"CMYK" => Space::Cmyk { device: true },
                b"Pattern" => Space::Pattern,
                other => {
                    let named = resources.get(b"ColorSpace").map(|d| doc.resolve(d)).and_then(|d| d.as_dict().and_then(|d| d.get(other).cloned()));
                    match named {
                        Some(v) => self.colour_space(&v, &Dict::new(), depth + 1),
                        None => Space::Unknown(1),
                    }
                }
            },
            Object::Array(a) => {
                let family = a.first().map(|f| doc.resolve(f)).and_then(|f| f.as_name().map(<[u8]>::to_vec)).unwrap_or_default();
                let arg = |i: usize| a.get(i).map(|o| doc.resolve(o));
                match family.as_slice() {
                    b"DeviceGray" | b"CalGray" | b"G" => Space::Gray,
                    b"DeviceRGB" | b"CalRGB" | b"RGB" => Space::Rgb,
                    b"DeviceCMYK" | b"CMYK" => Space::Cmyk { device: true },
                    b"Lab" => Space::Lab,
                    b"ICCBased" => {
                        let n = arg(1).and_then(|s| match s.as_ref() {
                            Object::Stream(s) => s.dict.get(b"N").and_then(|n| doc.resolve(n).as_int()),
                            _ => None,
                        });
                        match n {
                            Some(1) => Space::Gray,
                            Some(3) => Space::Rgb,
                            Some(4) => Space::Cmyk { device: false },
                            Some(n) => Space::Unknown(usize::try_from(n).unwrap_or(1).clamp(1, MAX_COMPONENTS)),
                            None => Space::Unknown(1),
                        }
                    }
                    b"Separation" => {
                        let Some(name) = arg(1).and_then(|n| n.as_name().map(<[u8]>::to_vec)) else { return Space::Unknown(1) };
                        let (alt, func) = (a.get(2).cloned(), a.get(3).cloned());
                        Space::Separation(self.target(&name, |w| w.spot("Separation", alt.as_ref(), func.as_ref())))
                    }
                    b"DeviceN" => match arg(1).as_deref().and_then(Object::as_array) {
                        Some(names) if !names.is_empty() && names.len() <= MAX_COMPONENTS => {
                            let names: Vec<Vec<u8>> =
                                names.iter().map(|n| doc.resolve(n).as_name().map(<[u8]>::to_vec).unwrap_or_default()).collect();
                            let (alt, func) = (a.get(2).cloned(), a.get(3).cloned());
                            // The attributes' Colorants dictionary defines spot colorants as Separations.
                            let colorants = arg(4)
                                .and_then(|attrs| attrs.as_dict().and_then(|d| d.get(b"Colorants").cloned()))
                                .map(|c| doc.resolve(&c))
                                .and_then(|c| c.as_dict().cloned());
                            let mut targets = Vec::with_capacity(names.len());
                            for name in &names {
                                let own = colorants.as_ref().and_then(|c| c.get(name).cloned()).map(|s| doc.resolve(&s)).and_then(|s| {
                                    let s = s.as_array()?;
                                    (s.first().map(|f| doc.resolve(f)).and_then(|f| f.as_name().map(<[u8]>::to_vec)).as_deref()
                                        == Some(b"Separation".as_slice()))
                                    .then(|| (s.get(2).cloned(), s.get(3).cloned()))
                                });
                                let target = match own {
                                    Some((alt, func)) => self.target(name, |w| w.spot("Separation", alt.as_ref(), func.as_ref())),
                                    None => self.target(name, |w| w.spot("DeviceN", alt.as_ref(), func.as_ref())),
                                };
                                targets.push(target);
                            }
                            Space::DeviceN(targets)
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
                    b"Pattern" => {
                        // An uncoloured pattern's underlying space still names inks.
                        if let Some(under) = a.get(1) {
                            let _ = self.colour_space(under, resources, depth + 1);
                        }
                        Space::Pattern
                    }
                    _ => Space::Unknown(1),
                }
            }
            _ => Space::Unknown(1),
        }
    }

    /// Register the inks that `resources` (and the forms, images, patterns and shadings it
    /// names) define, before painting: they decide how the cell budget is shared.
    fn declare(&mut self, resources: &Dict, depth: usize) {
        if depth >= MAX_DEPTH {
            return;
        }
        let doc = self.doc;
        let category = |key: &[u8]| -> Vec<Object> {
            resources
                .get(key)
                .map(|d| doc.resolve(d))
                .and_then(|d| d.as_dict().map(|d| d.iter().map(|(_, v)| v.clone()).take(MAX_DECLARED).collect()))
                .unwrap_or_default()
        };
        for cs in category(b"ColorSpace") {
            match self.declare_entry(&cs) {
                None => return,
                Some(false) => continue,
                Some(true) => {}
            }
            let _ = self.colour_space(&cs, &Dict::new(), 0);
        }
        let mut nested: Vec<Object> = Vec::new();
        for entry in category(b"XObject").into_iter().chain(category(b"Pattern")).chain(category(b"Shading")) {
            match self.declare_entry(&entry) {
                None => return,
                Some(false) => continue,
                Some(true) => {}
            }
            let object = doc.resolve(&entry);
            let dict = match object.as_ref() {
                Object::Stream(s) => &s.dict,
                Object::Dict(d) => d,
                _ => continue,
            };
            if let Some(cs) = dict.get(b"ColorSpace").cloned() {
                let _ = self.colour_space(&cs, resources, 0);
            }
            if let Some(shading) = dict.get(b"Shading").map(|s| doc.resolve(s))
                && let Some(cs) = match shading.as_ref() {
                    Object::Stream(s) => s.dict.get(b"ColorSpace").cloned(),
                    Object::Dict(d) => d.get(b"ColorSpace").cloned(),
                    _ => None,
                }
            {
                let _ = self.colour_space(&cs, resources, 0);
            }
            if let Some(r) = dict.get(b"Resources").cloned() {
                nested.push(r);
            }
        }
        for r in nested {
            let r = doc.resolve(&r);
            if let Some(d) = r.as_dict() {
                self.declare(d, depth + 1);
            }
        }
    }

    /// Count a resource entry while declaring: `None` once the budget is spent, `Some(false)`
    /// for an object already looked at (by reference), else `Some(true)`.
    fn declare_entry(&mut self, entry: &Object) -> Option<bool> {
        self.declared_entries = self.declared_entries.saturating_add(1);
        if self.declared_entries > MAX_DECLARED {
            self.truncated = true;
            return None;
        }
        Some(entry.as_ref().is_none_or(|r| self.declared.insert(r)))
    }

    /// Apply the overprint parameters of ExtGState `name`; note transparency it sets.
    fn ext_gstate(&mut self, name: &[u8], resources: &Dict, gs: &mut Gs) {
        let entry = resources.get(b"ExtGState").map(|d| self.doc.resolve(d)).and_then(|d| d.as_dict().and_then(|d| d.get(name).cloned()));
        let Some(entry) = entry else { return };
        let d = self.doc.resolve(&entry);
        let Some(d) = d.as_dict() else { return };
        let flag = |k: &[u8]| d.get(k).map(|v| matches!(self.doc.resolve(v).as_ref(), Object::Bool(true)));
        let (stroke, fill) = (flag(b"OP"), flag(b"op"));
        if let Some(s) = stroke {
            gs.op_stroke = s;
            // Without `op`, `OP` sets both.
            if fill.is_none() {
                gs.op_fill = s;
            }
        }
        if let Some(f) = fill {
            gs.op_fill = f;
        }
        if let Some(m) = d.get(b"OPM").and_then(|v| self.doc.resolve(v).as_int()) {
            gs.opm = if m == 1 { 1 } else { 0 };
        }
        let alpha = |k: &[u8]| d.get(k).and_then(|v| self.doc.resolve(v).as_f64()).is_some_and(|a| a < 1.0);
        let smask = d.get(b"SMask").is_some_and(|m| self.doc.resolve(m).as_name() != Some(b"None"));
        let blend = d.get(b"BM").map(|b| self.doc.resolve(b)).is_some_and(|b| match b.as_ref() {
            Object::Name(n) => !matches!(n.as_slice(), b"Normal" | b"Compatible"),
            Object::Array(a) => a.first().and_then(Object::as_name).is_some_and(|n| !matches!(n, b"Normal" | b"Compatible")),
            _ => false,
        });
        if alpha(b"CA") || alpha(b"ca") || smask || blend {
            self.approximate("transparency painted as opaque");
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
                        if let Some(w) = self.paint(&gs.fill, gs.op_fill, gs.opm) {
                            self.grid.fill(&path, even_odd, gs.clip, &w);
                        }
                    }
                    if matches!(name, b"S" | b"s" | b"B" | b"B*" | b"b" | b"b*")
                        && let Some(w) = self.paint(&gs.stroke, gs.op_stroke, gs.opm)
                    {
                        self.stroke(&path, &closed, &gs, &w);
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
                        _ => (Space::Cmyk { device: true }, 4),
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
                b"gs" => {
                    if let Some(name) = op.name(0) {
                        self.ext_gstate(name, resources, &mut gs);
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
    fn stroke(&mut self, path: &[Vec<P>], closed: &[bool], gs: &Gs, writes: &[(usize, u16)]) {
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
                self.grid.fill(&[square], false, gs.clip, writes);
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
                self.grid.fill(&[quad], false, gs.clip, writes);
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
        let (paint, overprint) = if paints.1 { (&gs.stroke, gs.op_stroke) } else { (&gs.fill, gs.op_fill) };
        if paints.0 && paints.1 {
            // Count the fill too, so exact/converted totals cover both inks.
            let _ = self.paint(&gs.fill, gs.op_fill, gs.opm);
        }
        if let Some(w) = self.paint(paint, overprint, gs.opm) {
            self.grid.fill(&[quad], false, gs.clip, &w);
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
        let doc = self.doc;
        let int = |k: &[u8]| dict.get(k).and_then(|o| doc.resolve(o).as_int());
        let mask = dict.get(b"ImageMask").is_some_and(|o| matches!(doc.resolve(o).as_ref(), Object::Bool(true)));
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
            let _ = self.paint(&Paint { space: Rc::new(Space::Unknown(1)), comps: Vec::new(), pattern: false }, false, 0);
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
            .map(|o| doc.resolve(o))
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
        let overprint = gs.op_fill && self.overprint;
        // A stencil mask paints the fill colour; other images their own pixels, all counted as
        // one painted object (converted when the space is).
        let fill_writes = if mask {
            match self.paint(&gs.fill, gs.op_fill, gs.opm) {
                Some(w) => w,
                None => return,
            }
        } else {
            let mut probe = Vec::new();
            let Some(kind) = space.ink_into(&space.initial(), self.conversion, &mut probe) else { return };
            self.paints = self.paints.saturating_add(1);
            if self.paints > MAX_PAINTS {
                self.truncated = true;
                return;
            }
            if kind.converted {
                self.converted = self.converted.saturating_add(1);
            } else {
                self.exact = self.exact.saturating_add(1);
            }
            if space.untracked() {
                self.unmeasure("inks beyond the tracked limit");
            }
            Vec::new()
        };
        let device = gs.ctm.then(&self.to_grid);
        let Some(inverse) = device.invert() else { return };
        let bbox = intersect(device.bbox([0.0, 0.0, 1.0, 1.0]), gs.clip);
        if !bbox.iter().all(|v| v.is_finite()) {
            return;
        }
        let (x0, x1) = Grid::span(bbox[0], bbox[2], self.grid.w);
        let (y0, y1) = Grid::span(bbox[1], bbox[3], self.grid.h);
        let plates = self.grid.plates.len().max(1) as u64;
        if x0 >= x1 || y0 >= y1 || !self.grid.spend(((x1 - x0) as u64).saturating_mul((y1 - y0) as u64).saturating_mul(plates)) {
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
        let mut values = std::mem::take(&mut self.values);
        let mut writes: Vec<(usize, u16)> = Vec::new();
        for row in y0..y1 {
            for col in x0..x1 {
                let (u, v) = inverse.apply(col as f64 + 0.5, row as f64 + 0.5);
                if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {
                    continue;
                }
                let px = ((u * w as f64) as usize).min(w - 1);
                let py = (((1.0 - v) * h as f64) as usize).min(h - 1);
                let at = row * self.grid.w + col;
                if mask {
                    // A stencil mask paints the fill colour where the sample decodes to 0.
                    let Some(s) = sample(px, py, 0) else { continue };
                    if s >= 0.5 {
                        continue;
                    }
                    self.grid.put(at, &fill_writes);
                    continue;
                }
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
                let Some(kind) = comps.get(..n).and_then(|c| space.ink_into(c, self.conversion, &mut values)) else { continue };
                self.grid.build(&values, kind.scope, overprint, gs.opm, &mut writes);
                self.grid.put(at, &writes);
            }
        }
        self.values = values;
    }
}

fn round_tint(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}
