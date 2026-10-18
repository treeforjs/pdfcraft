//! Output Preview's separations: list a document's inks, render plates, and sample the ink
//! percentages at a point (`print_separations_*`).

use pdfcraft_engine::preflight::Separations;
use pdfcraft_render::PageInfo;
use serde_json::{Value, json};

use crate::{Args, Automation, Content, Result, ToolError, failed};

fn bad(m: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs(m.into())
}

/// The default plate resolution, and the range accepted (the ink rasterizer's).
const DEFAULT_DPI: f64 = 72.0;
const MIN_DPI: f64 = pdfcraft_engine::pdfa::coverage::MIN_DPI;
const MAX_DPI: f64 = pdfcraft_engine::pdfa::coverage::MAX_DPI;
/// Plates returned as images in one result without `path`.
const MAX_INLINE_PLATES: usize = 8;

/// A user-space point as the displayed-page point every tool reports (origin top-left after
/// `/Rotate`, rounded to 1/100 pt).
pub(crate) fn view_point(page: Option<&PageInfo>, p: [f64; 2]) -> Value {
    let round = |v: f32| (v * 100.0).round() / 100.0;
    match page {
        Some(info) => {
            let [x, y] = info.user_to_view(p[0] as f32, p[1] as f32);
            json!([round(x), round(y)])
        }
        None => json!(p),
    }
}

fn hex(c: [f64; 3]) -> String {
    let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02X}{:02X}{:02X}", b(c[0]), b(c[1]), b(c[2]))
}

/// A colorant name made safe for a file name.
fn file_part(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).take(64).collect();
    if s.is_empty() { "ink".into() } else { s }
}

/// Turn a top-down image by the page's `/Rotate` (clockwise), as the page is displayed.
fn rotate(data: &[u8], w: usize, h: usize, channels: usize, degrees: u16) -> (Vec<u8>, usize, usize) {
    if !matches!(degrees, 90 | 180 | 270) {
        return (data.to_vec(), w, h);
    }
    let (nw, nh) = if degrees == 180 { (w, h) } else { (h, w) };
    let mut out = vec![255u8; nw.saturating_mul(nh).saturating_mul(channels)];
    for y in 0..h {
        for x in 0..w {
            let (nx, ny) = match degrees {
                90 => (h - 1 - y, x),
                180 => (w - 1 - x, h - 1 - y),
                _ => (y, w - 1 - x),
            };
            let (from, to) = ((y * w + x) * channels, (ny * nw + nx) * channels);
            if let (Some(src), Some(dst)) = (data.get(from..from + channels), out.get_mut(to..to + channels)) {
                dst.copy_from_slice(src);
            }
        }
    }
    (out, nw, nh)
}

fn encode(data: &[u8], w: usize, h: usize, color: png::ColorType) -> Result<Vec<u8>> {
    let (Ok(w32), Ok(h32)) = (u32::try_from(w), u32::try_from(h)) else { return Err(failed("the plate is too large to encode")) };
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w32, h32);
    enc.set_color(color);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(failed)?;
    writer.write_image_data(data).map_err(failed)?;
    writer.finish().map_err(failed)?;
    Ok(out)
}

fn ink_json(info: &pdfcraft_engine::preflight::InkInfo) -> Value {
    json!({
        "name": info.name,
        "kind": if info.process { "process" } else { "spot" },
        "defined_by": info.defined_by,
        "alternate": info.alternate,
        "tint_transform": info.tint_transform,
        "alternate_value": info.alternate_value,
        "preview": hex(info.preview),
        "preview_exact": info.preview_exact,
    })
}

impl Automation {
    fn dpi_arg(a: &Args) -> Result<f64> {
        let dpi = a.opt_num("dpi")?.unwrap_or(DEFAULT_DPI);
        if !(MIN_DPI..=MAX_DPI).contains(&dpi) {
            return Err(bad(format!("dpi must be between {MIN_DPI} and {MAX_DPI}")));
        }
        Ok(dpi)
    }

    /// The page's separations, read-only.
    fn separate(&self, a: &Args) -> Result<(usize, Separations)> {
        let page = self.page(a)?;
        let dpi = Self::dpi_arg(a)?;
        let overprint = a.opt_bool("overprint")?.unwrap_or(true);
        let seps = self.doc(a)?.separations(page, dpi, overprint).map_err(failed)?;
        Ok((page, seps))
    }

    /// Output Preview's separations list.
    pub(crate) fn separations_list(&self, a: &Args) -> Result<Value> {
        let doc = self.doc(a)?;
        let pages = if a.get("pages").is_some() { self.pages(a, "pages")? } else { Vec::new() };
        let max = pdfcraft_engine::pdfa::separations::MAX_LIST_PAGES;
        if pages.len() > max {
            return Err(bad(format!("pages may list at most {max} pages per call")));
        }
        let list = doc.separation_inks(&pages).map_err(failed)?;
        let one_based = |p: &[usize]| p.iter().map(|p| p + 1).collect::<Vec<_>>();
        let inks: Vec<Value> = list
            .inks
            .iter()
            .map(|u| {
                let mut v = ink_json(&u.info);
                v["pages"] = json!(one_based(&u.pages));
                v["declared_pages"] = json!(one_based(&u.declared_pages));
                v["used"] = json!(!u.pages.is_empty());
                v
            })
            .collect();
        let count = doc.info.pages.len();
        Ok(json!({
            "doc": doc.id.0,
            "inks": inks,
            "process_inks": list.inks.iter().filter(|u| u.info.process).count(),
            "spot_inks": list.inks.iter().filter(|u| !u.info.process).count(),
            "pages_checked": one_based(&list.pages),
            "pages_truncated": pages.is_empty() && count > max,
            "converted_pages": one_based(&list.converted_pages),
            "conversion": list.conversion,
            "unmeasured": list.unmeasured,
            "truncated_pages": one_based(&list.truncated_pages),
            "method": pdfcraft_engine::pdfa::coverage::METHOD,
        }))
    }

    /// Render plates (or a composite of the shown inks) as PNG.
    pub(crate) fn separations_render(&self, a: &Args) -> Result<Vec<Content>> {
        let (page, seps) = self.separate(a)?;
        let doc = self.doc(a)?;
        let info = doc.info.pages.get(page);
        let rotation = info.map_or(0, |p| p.rotation);
        let names: Vec<String> = match a.get("inks") {
            None => seps.inks.iter().map(|i| i.name.clone()).collect(),
            Some(Value::Array(list)) if !list.is_empty() => list
                .iter()
                .map(|v| v.as_str().map(str::to_string).ok_or_else(|| bad("inks must be an array of ink names")))
                .collect::<Result<Vec<_>>>()?,
            Some(_) => return Err(bad("inks must be a non-empty array of ink names (see print_separations_list)")),
        };
        let mut chosen: Vec<usize> = Vec::new();
        for name in &names {
            let Some(i) = seps.ink(name) else {
                let known: Vec<&str> = seps.inks.iter().map(|i| i.name.as_str()).collect();
                return Err(bad(format!("page {} has no ink {name:?}; its inks are {}", page + 1, known.join(", "))));
            };
            if !chosen.contains(&i) {
                chosen.push(i);
            }
        }
        let composite = a.opt_bool("composite")?.unwrap_or(false);
        let path = a.opt_str("path")?;
        if path.is_some_and(str::is_empty) {
            return Err(bad("path must not be empty"));
        }
        if !composite && path.is_none() && chosen.len() > MAX_INLINE_PLATES {
            return Err(bad(format!(
                "{} plates requested: pass path to write them as files, or choose at most {MAX_INLINE_PLATES} inks",
                chosen.len()
            )));
        }
        let (w, h) = (seps.width, seps.height);
        let mut images: Vec<Content> = Vec::new();
        let mut plates: Vec<Value> = Vec::new();
        let mut out_size = (w, h);
        let mut emit = |data: Vec<u8>, channels: usize, file: Option<String>, mut entry: Value| -> Result<Value> {
            let (data, rw, rh) = rotate(&data, w, h, channels, rotation);
            out_size = (rw, rh);
            let color = if channels == 1 { png::ColorType::Grayscale } else { png::ColorType::Rgb };
            let bytes = encode(&data, rw, rh, color)?;
            match file {
                Some(f) => {
                    let written = self.write_output(&f, &bytes)?;
                    entry["path"] = json!(written.display().to_string());
                }
                None => {
                    let (Ok(width), Ok(height)) = (u32::try_from(rw), u32::try_from(rh)) else { return Err(failed("the plate is too large")) };
                    images.push(Content::Png { data: bytes, width, height });
                }
            }
            Ok(entry)
        };
        let totals = |ink: usize| -> (f64, f64) {
            let (mut max, mut inked) = (0.0f64, 0usize);
            for row in 0..h {
                for col in 0..w {
                    let v = seps.value(ink, col, row);
                    max = max.max(v);
                    if v > 0.0 {
                        inked += 1;
                    }
                }
            }
            let cells = w.saturating_mul(h).max(1);
            (max, (inked as f64 / cells as f64 * 10_000.0).round() / 100.0)
        };
        let mut composite_entry = Value::Null;
        if composite {
            let entry = json!({ "shown": chosen.iter().filter_map(|i| seps.inks.get(*i).map(|x| x.name.clone())).collect::<Vec<_>>() });
            composite_entry = emit(seps.composite(&chosen), 3, path.map(str::to_string), entry)?;
        } else {
            let stem = path.map(|p| p.strip_suffix(".png").unwrap_or(p).to_string());
            for &i in &chosen {
                let Some(ink) = seps.inks.get(i) else { continue };
                let Some(gray) = seps.plate_gray(i) else { continue };
                let (max, area) = totals(i);
                let entry = json!({ "ink": ink.name, "kind": if ink.process { "process" } else { "spot" }, "max": max, "inked_percent": area });
                let file = stem.as_ref().map(|s| format!("{s}-{}.png", file_part(&ink.name)));
                plates.push(emit(gray, 1, file, entry)?);
            }
        }
        let mut out = vec![Content::Json(json!({
            "doc": doc.id.0,
            "page": page + 1,
            "dpi": (seps.dpi * 100.0).round() / 100.0,
            "width": out_size.0,
            "height": out_size.1,
            "rotation": rotation,
            "overprint": seps.overprint,
            "plates": plates,
            "composite": composite_entry,
            "encoding": if composite { "RGB: each shown ink multiplies the paper by its preview colour" } else { "8-bit gray: 0 (black) is 100% ink, 255 (white) none" },
            "inks": seps.inks.iter().map(ink_json).collect::<Vec<_>>(),
            "conversion": seps.conversion,
            "converted_paints": seps.converted_paints,
            "unmeasured": seps.unmeasured,
            "approximated": seps.approximated,
            "truncated": seps.truncated,
        }))];
        out.extend(images);
        Ok(out)
    }

    /// The ink percentages at one point.
    pub(crate) fn separations_sample(&self, a: &Args) -> Result<Value> {
        let x = a.opt_num("x")?.ok_or_else(|| bad("missing argument x"))?;
        let y = a.opt_num("y")?.ok_or_else(|| bad("missing argument y"))?;
        let (page, seps) = self.separate(a)?;
        let doc = self.doc(a)?;
        let info = doc.info.pages.get(page);
        let (w, h) = info.map_or((0.0, 0.0), |p| (f64::from(p.width), f64::from(p.height)));
        if !(0.0..=w).contains(&x) || !(0.0..=h).contains(&y) {
            return Err(bad(format!("the point ({x}, {y}) is outside page {} ({w} × {h} pt, origin top-left)", page + 1)));
        }
        let user = info.map_or([x, y], |p| {
            let [ux, uy] = p.view_to_user(x as f32, y as f32);
            [f64::from(ux), f64::from(uy)]
        });
        let s = pdfcraft_engine::pdfa::separations::sample(&seps, user[0], user[1])
            .ok_or_else(|| bad(format!("the point ({x}, {y}) is outside page {}'s crop box", page + 1)))?;
        let rect = |r: [f64; 4]| -> Value {
            let r32 = r.map(|v| v as f32);
            json!(info.map_or(r32, |p| crate::comments::rect_to_view(p, r32)))
        };
        Ok(json!({
            "doc": doc.id.0,
            "page": page + 1,
            "point": [x, y],
            "pdf_point": [(user[0] * 100.0).round() / 100.0, (user[1] * 100.0).round() / 100.0],
            "inks": s.inks.iter().map(|(name, v)| json!({ "name": name, "percent": v })).collect::<Vec<_>>(),
            "total": s.total,
            "cell": rect(s.cell),
            "pdf_cell": s.cell,
            "dpi": (seps.dpi * 100.0).round() / 100.0,
            "overprint": seps.overprint,
            "conversion": seps.conversion,
            "unmeasured": seps.unmeasured,
            "truncated": seps.truncated,
        }))
    }
}
