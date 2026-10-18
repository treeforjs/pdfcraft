//! Overprint Preview (Acrobat: Preferences ▸ Page Display ▸ Use Overprint Preview, and Output
//! Preview's Simulate Overprinting): how objects painted with overprint on (`OP` for strokes,
//! `op` for fills, text and images, with overprint mode `OPM`) combine with what is below them
//! on press, instead of knocking it out as the normal render does.
//!
//! The page renderer is RGB-only and always knocks out, so the simulation corrects its pixels
//! with the ink plates of [`crate::coverage`]: the page is separated three times on a grid at
//! about the render's resolution — with overprint simulated, with every object knocked out, and
//! without the overprinting objects (their backdrop) — and only the cells where overprint
//! changes some plate are touched. There each plate set is composited to RGB (process inks
//! through the conversion's [`Conversion::display_cmyk`] — the renderer's CMYK conversion, under
//! the engine's colour settings — so the result matches the rest of the page; spot inks
//! multiply the paper by their preview colour).
//! For every pixel of such a cell the rendered colour gives the overprinting object's coverage
//! (its position between the backdrop and the knocked-out colour), so anti-aliased edges and
//! text glyphs keep the renderer's geometry: the pixel moves by that coverage times the
//! difference between the overprinted and the knocked-out colour.
//!
//! What the ink rasterizer can't see (patterns, shadings, DCT/JPX/JBIG2/CCITT images,
//! annotations) is neither simulated nor counted as backdrop; transparency is painted opaque.
//! The simulation reports them with the plates' `unmeasured` and `approximated` counts.

use std::collections::BTreeMap;

use pdfcraft_cos::Document;

use crate::colour_warnings::{self, Finding, Kind};
use crate::coverage::{Conversion, InkInfo, MAX_DPI, MIN_DPI, PROCESS_INKS, Raster, rasterize, rasterize_without_overprinting};

/// The default grid resolution for listing overprinting objects (cells per inch).
pub const DEFAULT_OBJECTS_DPI: f64 = 72.0;
/// Overprinting objects listed per page by default, and at most.
pub const DEFAULT_MAX_OBJECTS: usize = 100;
pub const MAX_OBJECTS: usize = 10_000;

/// When the normal page render simulates overprint (Acrobat's "Use Overprint Preview").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Never: overprinting objects knock out, as on screen in most viewers.
    Never,
    /// Always.
    Always,
    /// Only for PDF/X files (Acrobat's default): documents that declare a PDF/X version or
    /// carry a `GTS_PDFX` output intent.
    #[default]
    PdfX,
    /// Automatic: whenever the page has objects painted with overprint on.
    Automatic,
}

impl Mode {
    pub const ALL: [Mode; 4] = [Mode::Never, Mode::Always, Mode::PdfX, Mode::Automatic];

    pub fn id(self) -> &'static str {
        match self {
            Mode::Never => "never",
            Mode::Always => "always",
            Mode::PdfX => "pdfx",
            Mode::Automatic => "automatic",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::Never => "Never",
            Mode::Always => "Always",
            Mode::PdfX => "Only For PDF/X Files",
            Mode::Automatic => "Automatic",
        }
    }

    pub fn from_id(id: &str) -> Option<Mode> {
        Mode::ALL.into_iter().find(|m| m.id().eq_ignore_ascii_case(id.trim()))
    }

    /// Whether pages of `doc` are rendered with overprint simulated. For [`Mode::Automatic`]
    /// this is true too: the simulation finds out per page whether anything overprints and
    /// leaves pages without overprinting objects as they render.
    pub fn applies(self, doc: &Document) -> bool {
        match self {
            Mode::Never => false,
            Mode::Always | Mode::Automatic => true,
            Mode::PdfX => is_pdfx(doc),
        }
    }
}

/// Whether `doc` is a PDF/X file: it declares `GTS_PDFXVersion` (document information or XMP)
/// or its output intent is a `GTS_PDFX` one.
pub fn is_pdfx(doc: &Document) -> bool {
    crate::pdfx::declared_pdfx(doc).0.is_some() || crate::separations::output_intent(doc).is_some_and(|i| i.subtype == "GTS_PDFX")
}

/// Where overprint changes a page: the separated grid and, for each cell where some plate
/// differs from the knocked-out page, its colours.
#[derive(Clone, Debug)]
pub struct Difference {
    /// 0-based page.
    pub page: usize,
    /// The separated area (the crop box) in user space.
    pub crop: [f64; 4],
    /// The grid resolution used (cells per inch) and its size; row 0 is the bottom.
    pub dpi: f64,
    pub width: usize,
    pub height: usize,
    /// Objects painted with overprint on.
    pub overprint_paints: usize,
    /// Changed cells (`row * width + col`), ascending.
    pub cells: Vec<u32>,
    /// Their colours (sRGB, 0–1): overprinted, knocked out and the backdrop without the
    /// overprinting objects (empty unless asked for).
    pub overprinted: Vec<[f32; 3]>,
    pub knocked_out: Vec<[f32; 3]>,
    pub backdrop: Vec<[f32; 3]>,
    /// How RGB and Lab colours became inks.
    pub conversion: String,
    pub unmeasured: BTreeMap<&'static str, usize>,
    pub approximated: BTreeMap<&'static str, usize>,
    pub truncated: bool,
}

impl Difference {
    /// The cell size in points.
    pub fn cell(&self) -> f64 {
        72.0 / self.dpi
    }

    /// Changed cells as a percentage of the page.
    pub fn changed_percent(&self) -> f64 {
        let all = self.width.saturating_mul(self.height).max(1) as f64;
        (self.cells.len() as f64 / all * 10_000.0).round() / 100.0
    }

    /// The position in [`Self::cells`] of grid cell (`col`, `row`), when it changed.
    fn find(&self, col: usize, row: usize) -> Option<usize> {
        if col >= self.width || row >= self.height {
            return None;
        }
        let at = u32::try_from(row.checked_mul(self.width)?.checked_add(col)?).ok()?;
        self.cells.binary_search(&at).ok()
    }

    /// The grid cell holding user-space point (`x`, `y`) (clamped to the grid).
    fn cell_of(&self, x: f64, y: f64) -> Option<(usize, usize)> {
        if !(x.is_finite() && y.is_finite()) || self.width == 0 || self.height == 0 {
            return None;
        }
        let c = self.cell();
        let col = ((x - self.crop[0]) / c).floor().clamp(0.0, (self.width - 1) as f64) as usize;
        let row = ((y - self.crop[1]) / c).floor().clamp(0.0, (self.height - 1) as f64) as usize;
        Some((col, row))
    }

    /// Changed cells whose centres lie in the user-space box `[x0, y0, x1, y1]`.
    pub fn changed_in(&self, bbox: [f64; 4]) -> usize {
        let c = self.cell();
        if !bbox.iter().all(|v| v.is_finite()) || self.width == 0 || self.height == 0 {
            return 0;
        }
        let span = |lo: f64, hi: f64, origin: f64, n: usize| -> (usize, usize) {
            let a = ((lo.min(hi) - origin) / c - 0.5).ceil().max(0.0);
            let b = ((lo.max(hi) - origin) / c - 0.5).floor().min(n as f64 - 1.0);
            if b < a { (1, 0) } else { (a as usize, b as usize) }
        };
        let (c0, c1) = span(bbox[0], bbox[2], self.crop[0], self.width);
        let (r0, r1) = span(bbox[1], bbox[3], self.crop[1], self.height);
        if c1 < c0 || r1 < r0 {
            return 0;
        }
        // Few changed cells: count those inside; else look each cell of the box up.
        let area = (c1 - c0 + 1).saturating_mul(r1 - r0 + 1);
        if self.cells.len() < area {
            self.cells
                .iter()
                .filter(|&&i| {
                    let (col, row) = (i as usize % self.width, i as usize / self.width);
                    (c0..=c1).contains(&col) && (r0..=r1).contains(&row)
                })
                .count()
        } else {
            (r0..=r1).map(|row| (c0..=c1).filter(|&col| self.find(col, row).is_some()).count()).sum()
        }
    }
}

/// The sRGB preview of each ink for compositing: process inks go through the renderer's CMYK
/// conversion as a whole, so only spot inks need one (their CMYK alternate at 100 % converted
/// the same way, else [`InkInfo::preview`]).
fn spot_previews(inks: &[InkInfo], conversion: &dyn Conversion) -> Vec<[f32; 3]> {
    inks.iter()
        .map(|info| {
            if let Some([c, m, y, k]) = info.alternate_value.as_deref() {
                let q = [*c, *m, *y, *k].map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
                if let Some(rgb) = conversion.display_cmyk(&q)
                    && let [r, g, b] = rgb.as_slice()
                {
                    return [*r, *g, *b].map(|v| f32::from(v) / 255.0);
                }
            }
            info.preview.map(|v| v.clamp(0.0, 1.0) as f32)
        })
        .collect()
}

/// The composite colour of `raster` at each of `cells`.
fn composite(raster: &Raster, cells: &[u32], conversion: &dyn Conversion) -> Result<Vec<[f32; 3]>, String> {
    let w = raster.width.max(1);
    let previews = spot_previews(&raster.inks, conversion);
    let mut quads: Vec<u8> = Vec::new();
    quads.try_reserve_exact(cells.len().saturating_mul(4)).map_err(|_| "not enough memory for the overprint preview".to_string())?;
    for &i in cells {
        let (col, row) = (i as usize % w, i as usize / w);
        for ink in 0..PROCESS_INKS.len() {
            quads.push((raster.value(ink, col, row) / 100.0 * 255.0).round().clamp(0.0, 255.0) as u8);
        }
    }
    let rgb = conversion.display_cmyk(&quads).ok_or("the CMYK conversion failed")?;
    let mut out = Vec::new();
    out.try_reserve_exact(cells.len()).map_err(|_| "not enough memory for the overprint preview".to_string())?;
    for (n, &i) in cells.iter().enumerate() {
        let (col, row) = (i as usize % w, i as usize / w);
        let base = n.saturating_mul(3);
        let mut c = [0usize, 1, 2].map(|k| rgb.get(base + k).map_or(1.0, |v| f32::from(*v) / 255.0));
        for (ink, preview) in previews.iter().enumerate().skip(PROCESS_INKS.len()) {
            let t = (raster.value(ink, col, row) / 100.0).clamp(0.0, 1.0) as f32;
            if t > 0.0 {
                for (v, p) in c.iter_mut().zip(preview) {
                    *v *= 1.0 - t * (1.0 - p);
                }
            }
        }
        out.push(c);
    }
    Ok(out)
}

/// Where overprint changes 0-based `page`, separated at `dpi` (lowered on large pages or with
/// many inks); with `backdrop` also the colours without the overprinting objects. A page
/// without overprinting objects is separated once.
pub fn difference(doc: &Document, page: usize, dpi: f64, backdrop: bool, conversion: &dyn Conversion) -> Result<Difference, String> {
    let dpi = if dpi.is_finite() { dpi.clamp(MIN_DPI, MAX_DPI) } else { MIN_DPI };
    let op = rasterize(doc, page, dpi, true, conversion)?;
    let mut out = Difference {
        page,
        crop: op.crop,
        dpi: op.dpi,
        width: op.width,
        height: op.height,
        overprint_paints: op.overprint_paints,
        cells: Vec::new(),
        overprinted: Vec::new(),
        knocked_out: Vec::new(),
        backdrop: Vec::new(),
        conversion: op.conversion.clone(),
        unmeasured: op.unmeasured.clone(),
        approximated: op.approximated.clone(),
        truncated: op.truncated,
    };
    if op.overprint_paints == 0 {
        return Ok(out);
    }
    // The knocked-out page is separated at the resolution the first pass chose, so the grids
    // match.
    let ko = rasterize(doc, page, op.dpi, false, conversion)?;
    let names = |r: &Raster| r.inks.iter().map(|i| i.name.clone()).collect::<Vec<_>>();
    if (ko.width, ko.height) != (op.width, op.height) || names(&ko) != names(&op) {
        return Err("the page separated differently with and without overprint".into());
    }
    let (w, h, inks) = (op.width, op.height, op.inks.len());
    if u32::try_from(w.saturating_mul(h)).is_err() {
        return Err("the separation grid is too large".into());
    }
    for row in 0..h {
        for col in 0..w {
            if (0..inks).any(|i| op.value(i, col, row) != ko.value(i, col, row)) {
                out.cells.push((row * w + col) as u32);
            }
        }
    }
    out.truncated |= ko.truncated;
    if out.cells.is_empty() {
        return Ok(out);
    }
    out.knocked_out = composite(&ko, &out.cells, conversion)?;
    drop(ko);
    out.overprinted = composite(&op, &out.cells, conversion)?;
    drop(op);
    if backdrop {
        let under = rasterize_without_overprinting(doc, page, out.dpi, conversion)?;
        if (under.width, under.height) != (w, h) {
            return Err("the page separated differently without its overprinting objects".into());
        }
        out.truncated |= under.truncated;
        out.backdrop = composite(&under, &out.cells, conversion)?;
    }
    Ok(out)
}

/// What [`simulate`] did.
#[derive(Clone, Debug, PartialEq)]
pub struct Simulation {
    /// 0-based page.
    pub page: usize,
    /// The ink grid's resolution (cells per inch).
    pub dpi: f64,
    /// Objects painted with overprint on.
    pub overprint_paints: usize,
    /// Grid cells where overprint changes the plates, and their share of the page in percent.
    pub changed_cells: usize,
    pub changed_percent: f64,
    /// Pixels whose colour changed.
    pub changed_pixels: usize,
    pub conversion: String,
    pub unmeasured: BTreeMap<&'static str, usize>,
    pub approximated: BTreeMap<&'static str, usize>,
    pub truncated: bool,
}

/// Simulate overprint on a normal render of 0-based `page`: `rgba` is `width × height` RGBA
/// pixels, top row first, covering the crop box as displayed after `rotation` (0, 90, 180 or
/// 270 degrees clockwise). Pixels change only where overprint changes some plate.
pub fn simulate(
    doc: &Document,
    page: usize,
    rotation: u16,
    rgba: &mut [u8],
    width: usize,
    height: usize,
    conversion: &dyn Conversion,
) -> Result<Simulation, String> {
    if width == 0 || height == 0 || rgba.len() != width.saturating_mul(height).saturating_mul(4) {
        return Err("the rendered image does not match its size".into());
    }
    let pages = pdfcraft_model::pages(doc);
    let p = pages.get(page).ok_or_else(|| format!("page {} does not exist; the document has {} pages", page.saturating_add(1), pages.len()))?;
    let crop = p.crop(doc);
    let (cw, ch) = (crop[2] - crop[0], crop[3] - crop[1]);
    if !(cw.is_finite() && ch.is_finite() && cw > 0.0 && ch > 0.0) {
        return Err("the page has an empty or invalid crop box".into());
    }
    let quarter = matches!(rotation, 90 | 270);
    let shown_width = if quarter { ch } else { cw };
    let dpi = width as f64 / shown_width * 72.0;
    let diff = difference(doc, page, dpi, true, conversion)?;
    let mut sim = Simulation {
        page,
        dpi: diff.dpi,
        overprint_paints: diff.overprint_paints,
        changed_cells: diff.cells.len(),
        changed_percent: diff.changed_percent(),
        changed_pixels: 0,
        conversion: diff.conversion.clone(),
        unmeasured: diff.unmeasured.clone(),
        approximated: diff.approximated.clone(),
        truncated: diff.truncated,
    };
    if diff.cells.is_empty() {
        return Ok(sim);
    }
    let [cx0, cy0, cx1, cy1] = diff.crop;
    for py in 0..height {
        let b = (py as f64 + 0.5) / height as f64;
        for px in 0..width {
            let a = (px as f64 + 0.5) / width as f64;
            let (u, v) = match rotation {
                90 => (b, 1.0 - a),
                180 => (1.0 - a, 1.0 - b),
                270 => (1.0 - b, a),
                _ => (a, b),
            };
            let (x, y) = (cx0 + u * (cx1 - cx0), cy1 - v * (cy1 - cy0));
            let Some((col, row)) = diff.cell_of(x, y) else { continue };
            let Some(n) = diff.find(col, row) else { continue };
            let (Some(over), Some(knock), Some(under)) = (diff.overprinted.get(n), diff.knocked_out.get(n), diff.backdrop.get(n)) else {
                continue;
            };
            let at = (py * width + px) * 4;
            let Some(pixel) = rgba.get_mut(at..at + 3) else { continue };
            let r = [0, 1, 2].map(|k| pixel.get(k).map_or(1.0, |v| f32::from(*v) / 255.0));
            // The overprinting object's coverage of this pixel: where the rendered colour lies
            // between the backdrop and the knocked-out colour.
            let d = [0, 1, 2].map(|k| knock[k] - under[k]);
            let norm: f32 = d.iter().map(|v| v * v).sum();
            let alpha = if norm > 1e-4 {
                let dot: f32 = (0..3).map(|k| (r[k] - under[k]) * d[k]).sum();
                (dot / norm).clamp(0.0, 1.0)
            } else {
                1.0
            };
            let mut changed = false;
            for (k, out) in pixel.iter_mut().enumerate() {
                let (Some(rk), Some(ok), Some(kk)) = (r.get(k), over.get(k), knock.get(k)) else { continue };
                let v = ((rk + alpha * (ok - kk)).clamp(0.0, 1.0) * 255.0).round() as u8;
                changed |= v != *out;
                *out = v;
            }
            if changed {
                sim.changed_pixels = sim.changed_pixels.saturating_add(1);
            }
        }
    }
    Ok(sim)
}

/// One object (or group of identical ones) painted with overprint on.
#[derive(Clone, Debug, PartialEq)]
pub struct OverprintObject {
    /// The colour-warning finding describing it (`overprint-paint`, or `overprint-white` for a
    /// white or zero-tint one).
    pub finding: Finding,
    /// Grid cells inside its box where overprint changes the plates (`None` without a box).
    pub changed_cells: Option<usize>,
}

impl OverprintObject {
    /// Whether overprinting visibly changes the page inside its box.
    pub fn changes_appearance(&self) -> Option<bool> {
        self.changed_cells.map(|n| n > 0)
    }
}

/// The objects of a page that overprint.
#[derive(Clone, Debug)]
pub struct OverprintObjects {
    /// 0-based page.
    pub page: usize,
    pub objects: Vec<OverprintObject>,
    /// ExtGStates applied on the page that turn overprint on or set OPM 1.
    pub states: Vec<Finding>,
    /// More objects were found than kept.
    pub objects_truncated: bool,
    /// Painted objects whose colour couldn't be classified, by reason.
    pub unresolved: BTreeMap<&'static str, usize>,
    /// Limits the content walk reached.
    pub limits: Vec<String>,
    /// Where overprint changes the page (without backdrop colours).
    pub difference: Difference,
}

/// List the objects of 0-based `page` painted with overprint on (at most `max_objects`), each
/// with the cells inside its box where overprint changes the plates (separated at `dpi`).
pub fn objects(doc: &Document, page: usize, dpi: f64, max_objects: usize, conversion: &dyn Conversion) -> Result<OverprintObjects, String> {
    if !(1..=MAX_OBJECTS).contains(&max_objects) {
        return Err(format!("max_objects must be between 1 and {MAX_OBJECTS}"));
    }
    if !dpi.is_finite() || !(MIN_DPI..=MAX_DPI).contains(&dpi) {
        return Err(format!("dpi must be between {MIN_DPI} and {MAX_DPI}"));
    }
    let options = colour_warnings::Options { pages: vec![page], max_findings: max_objects, ..colour_warnings::Options::default() };
    let report = colour_warnings::check(doc, &options)?;
    let difference = difference(doc, page, dpi, false, conversion)?;
    let found = report.count(Kind::OverprintPaint).saturating_add(report.count(Kind::OverprintWhite));
    let mut objects = Vec::new();
    let mut states = Vec::new();
    for f in report.findings {
        match f.kind {
            Kind::OverprintPaint | Kind::OverprintWhite => {
                let changed_cells = f.bbox.map(|b| difference.changed_in(b));
                objects.push(OverprintObject { finding: f, changed_cells });
            }
            Kind::OverprintState => states.push(f),
            _ => {}
        }
    }
    let objects_truncated = found > objects.len();
    Ok(OverprintObjects { page, objects, states, objects_truncated, unresolved: report.unresolved, limits: report.limits, difference })
}

#[cfg(test)]
mod tests;
