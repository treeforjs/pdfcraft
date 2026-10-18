//! Separations preview (Acrobat's Output Preview ▸ Separations): which inks a document uses —
//! the four process plates and every spot or DeviceN colorant, with its alternate space — on
//! which pages, and each plate's ink at any point of a page.
//!
//! The plates come from the ink rasterizer in [`crate::coverage`] (see there for what is exact,
//! what is converted and what isn't measured). Listing inks rasterizes each page at a coarse
//! resolution: an ink counts as *painted* on a page when some object puts a non-zero amount of
//! it there, and as *declared* when the page's resources (its forms, images, patterns and
//! shadings included) define it.
//!
//! RGB and Lab colours become process inks through a [`Conversion`]: [`output_intent`] finds
//! the document's output intent profile for the caller to build one from, else [`BuiltIn`].

use std::collections::BTreeMap;

use pdfcraft_cos::{Document, Object};

pub use crate::coverage::{BuiltIn, Conversion, InkInfo, MAX_DPI, MAX_INKS, MIN_DPI, PROCESS_INKS, Raster, rasterize};

/// The resolution inks are listed at (cells per inch).
pub const LIST_DPI: f64 = MIN_DPI;
/// Pages listed per call.
pub const MAX_LIST_PAGES: usize = 500;
/// Output intent profiles read at most.
pub const MAX_PROFILE: usize = 32 << 20;

/// One ink of the document.
#[derive(Clone, Debug, PartialEq)]
pub struct InkUse {
    pub info: InkInfo,
    /// 0-based pages where something puts this ink down.
    pub pages: Vec<usize>,
    /// 0-based pages whose resources define it (spot inks; empty for process inks).
    pub declared_pages: Vec<usize>,
}

/// The inks of the listed pages.
#[derive(Clone, Debug, PartialEq)]
pub struct InkList {
    /// The four process inks first, then spot inks in the order first met.
    pub inks: Vec<InkUse>,
    /// The pages looked at (0-based).
    pub pages: Vec<usize>,
    /// Pages where painted objects were converted from RGB/Lab (0-based).
    pub converted_pages: Vec<usize>,
    /// Objects that could not be measured, by reason, over all pages.
    pub unmeasured: BTreeMap<&'static str, usize>,
    /// Pages where a cap was reached (0-based).
    pub truncated_pages: Vec<usize>,
    /// How RGB and Lab colours were converted.
    pub conversion: String,
}

/// List the inks used on 0-based `pages` (all pages when empty, at most [`MAX_LIST_PAGES`]).
pub fn list_inks(doc: &Document, pages: &[usize], conversion: &dyn Conversion) -> Result<InkList, String> {
    let count = pdfcraft_model::pages(doc).len();
    let pages: Vec<usize> = if pages.is_empty() { (0..count.min(MAX_LIST_PAGES)).collect() } else { pages.to_vec() };
    if pages.len() > MAX_LIST_PAGES {
        return Err(format!("at most {MAX_LIST_PAGES} pages can be listed at once"));
    }
    if let Some(p) = pages.iter().find(|p| **p >= count) {
        return Err(format!("page {} does not exist; the document has {count} pages", p.saturating_add(1)));
    }
    let mut inks: Vec<InkUse> =
        (0..PROCESS_INKS.len()).map(|i| InkUse { info: InkInfo::process(i), pages: Vec::new(), declared_pages: Vec::new() }).collect();
    let mut out = InkList {
        inks: Vec::new(),
        pages: pages.clone(),
        converted_pages: Vec::new(),
        unmeasured: BTreeMap::new(),
        truncated_pages: Vec::new(),
        conversion: conversion.describe(),
    };
    for &page in &pages {
        let r = rasterize(doc, page, LIST_DPI, true, conversion)?;
        for (i, info) in r.inks.iter().enumerate() {
            let slot = match inks.iter().position(|u| u.info.name == info.name) {
                Some(slot) => slot,
                None => {
                    inks.push(InkUse { info: info.clone(), pages: Vec::new(), declared_pages: Vec::new() });
                    inks.len() - 1
                }
            };
            let Some(u) = inks.get_mut(slot) else { continue };
            if !info.process && !u.declared_pages.contains(&page) {
                u.declared_pages.push(page);
            }
            if r.painted.get(i).copied().unwrap_or(false) && !u.pages.contains(&page) {
                u.pages.push(page);
            }
        }
        if r.converted_paints > 0 {
            out.converted_pages.push(page);
        }
        for (why, n) in &r.unmeasured {
            let total = out.unmeasured.entry(why).or_insert(0);
            *total = total.saturating_add(*n);
        }
        if r.truncated {
            out.truncated_pages.push(page);
        }
    }
    out.inks = inks;
    Ok(out)
}

/// The ink percentages at one point of a page.
#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    /// Each ink of the page with its percentage at the point (0–100).
    pub inks: Vec<(String, f64)>,
    /// Their sum.
    pub total: f64,
    /// The sampled grid cell in user space `[x0, y0, x1, y1]`.
    pub cell: [f64; 4],
}

/// The inks of `raster` at user-space point (`x`, `y`), or `None` outside the crop box.
pub fn sample(raster: &Raster, x: f64, y: f64) -> Option<Sample> {
    let (col, row) = raster.cell_of(x, y)?;
    let round = |v: f64| (v * 100.0).round() / 100.0;
    let inks: Vec<(String, f64)> = raster.inks.iter().enumerate().map(|(i, info)| (info.name.clone(), round(raster.value(i, col, row)))).collect();
    let total = round(raster.total(col, row));
    let c = raster.cell();
    let (x0, y0) = (raster.crop[0] + col as f64 * c, raster.crop[1] + row as f64 * c);
    Some(Sample { inks, total, cell: [round(x0), round(y0), round((x0 + c).min(raster.crop[2])), round((y0 + c).min(raster.crop[3]))] })
}

/// A document's output intent: the destination profile RGB and Lab colours separate through.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputIntent {
    /// Its subtype (`GTS_PDFX`, `GTS_PDFA1`, …).
    pub subtype: String,
    /// `OutputConditionIdentifier`, or `OutputCondition`.
    pub condition: Option<String>,
    /// The `DestOutputProfile` bytes (decoded, at most [`MAX_PROFILE`]).
    pub profile: Option<Vec<u8>>,
    /// Why the profile could not be read, when it couldn't.
    pub problem: Option<String>,
}

/// The catalog's output intent: a `GTS_PDFX` one first, else the first with a profile, else the
/// first.
pub fn output_intent(doc: &Document) -> Option<OutputIntent> {
    let catalog = doc.get(doc.root()?).as_dict().cloned()?;
    let list = catalog.get(b"OutputIntents").map(|o| doc.resolve(o))?;
    let intents: Vec<pdfcraft_cos::Dict> = list.as_array()?.iter().take(64).filter_map(|o| doc.resolve(o).as_dict().cloned()).collect();
    let pick = intents
        .iter()
        .find(|d| d.name(b"S") == Some(b"GTS_PDFX"))
        .or_else(|| intents.iter().find(|d| d.get(b"DestOutputProfile").is_some()))
        .or_else(|| intents.first())?;
    let text = |key: &[u8]| -> Option<String> {
        match doc.resolve(pick.get(key)?).as_ref() {
            Object::String(s) => Some(String::from_utf8_lossy(&s.bytes).into_owned()).filter(|s| !s.is_empty()),
            _ => None,
        }
    };
    let (mut profile, mut problem) = (None, None);
    if let Some(p) = pick.get(b"DestOutputProfile") {
        match doc.resolve(p).as_ref() {
            Object::Stream(s) => match s.decoded_within(MAX_PROFILE) {
                Ok(bytes) => profile = Some(bytes),
                Err(e) => problem = Some(format!("the output profile could not be decoded: {e}")),
            },
            _ => problem = Some("DestOutputProfile is not a stream".into()),
        }
    }
    Some(OutputIntent {
        subtype: pick.name(b"S").map(|s| String::from_utf8_lossy(s).into_owned()).unwrap_or_default(),
        condition: text(b"OutputConditionIdentifier").or_else(|| text(b"OutputCondition")),
        profile,
        problem,
    })
}

#[cfg(test)]
mod tests;
