//! Output Preview: read and change a document's session settings (simulation profile,
//! simulate overprinting / paper colour / black ink, the Show filter, separations on and off,
//! the total area coverage warning) and render the preview as PNG with its coverage statistics
//! (`print_output_preview_*`).

use pdfcraft_engine::output_preview::{MAX_HIDDEN_INKS, MAX_INK_NAME, MAX_LIMIT, MIN_LIMIT, OutputPreview, Show, SimulationInfo, SimulationProfile};
use serde_json::{Value, json};

use crate::{Args, Automation, Content, Result, ToolError, failed};

fn bad(m: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs(m.into())
}

/// The default preview resolution, and the range accepted (the ink rasterizer's).
const DEFAULT_DPI: f64 = 72.0;
const MIN_DPI: f64 = pdfcraft_engine::pdfa::coverage::MIN_DPI;
const MAX_DPI: f64 = pdfcraft_engine::pdfa::coverage::MAX_DPI;

fn colour_hex(c: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

fn parse_colour(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().strip_prefix('#')?;
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let byte = |i: usize| h.get(i..i + 2).and_then(|b| u8::from_str_radix(b, 16).ok());
    Some([byte(0)?, byte(2)?, byte(4)?])
}

pub(crate) fn settings_json(s: &OutputPreview) -> Value {
    let profile = match &s.profile {
        SimulationProfile::Profile { id, name, .. } => json!({ "id": id, "name": name }),
        other => json!({ "id": other.id() }),
    };
    json!({
        "simulation_profile": profile,
        "simulate_overprint": s.simulate_overprint,
        "simulate_paper": s.simulate_paper,
        "simulate_black": s.simulate_black || s.simulate_paper,
        "show": s.show.id(),
        "show_label": s.show.label(),
        "hidden_inks": s.hidden_inks,
        "coverage_warning": s.coverage_warning,
        "coverage_limit": s.coverage_limit,
        "warning_color": colour_hex(s.warning_colour),
    })
}

fn simulation_json(i: &SimulationInfo) -> Value {
    json!({
        "source": i.source,
        "used": i.used,
        "name": i.name,
        "profile_used": i.profile_used,
        "display": i.display,
        "conversion": i.conversion,
        "problem": i.problem,
    })
}

fn shows() -> Vec<Value> {
    Show::ALL.iter().map(|s| json!({ "id": s.id(), "label": s.label() })).collect()
}

impl Automation {
    /// The inks Output Preview lists (after the Ink Manager), or `None` on very long documents.
    fn preview_ink_names(&self, a: &Args) -> Result<Option<Vec<String>>> {
        let doc = self.doc(a)?;
        if doc.info.pages.len() > pdfcraft_engine::pdfa::separations::MAX_LIST_PAGES {
            return Ok(None);
        }
        let list = doc.separation_inks(&[]).map_err(failed)?;
        Ok(Some(list.inks.iter().map(|u| u.info.name.clone()).collect()))
    }

    fn output_preview_state(&self, a: &Args) -> Result<Value> {
        let doc = self.doc(a)?;
        let s = doc.output_preview();
        let simulation = doc.output_preview_simulation().map_err(failed)?;
        let list = if doc.info.pages.len() <= pdfcraft_engine::pdfa::separations::MAX_LIST_PAGES {
            Some(doc.separation_inks(&[]).map_err(failed)?)
        } else {
            None
        };
        let one_based = |p: &[usize]| p.iter().map(|p| p + 1).collect::<Vec<_>>();
        let inks: Option<Vec<Value>> = list.as_ref().map(|l| {
            l.inks
                .iter()
                .map(|u| {
                    let mut v = crate::separations::ink_json(&u.info);
                    v["shown"] = json!(!s.hidden_inks.contains(&u.info.name));
                    v["pages"] = json!(one_based(&u.pages));
                    v["used"] = json!(!u.pages.is_empty());
                    v
                })
                .collect()
        });
        Ok(json!({
            "doc": doc.id.0,
            "settings": settings_json(s),
            "simulation": simulation_json(&simulation),
            "is_pdfx": doc.is_pdfx().unwrap_or(false),
            "inks": inks,
            "inks_listed": inks.is_some(),
            "show_options": shows(),
            "ink_manager": crate::ink_manager::settings_json(doc.ink_manager()),
        }))
    }

    /// Output Preview's settings, the simulation profile and the separations list.
    pub(crate) fn output_preview_get(&self, a: &Args) -> Result<Value> {
        self.output_preview_state(a)
    }

    /// Change Output Preview's settings for a document (session only).
    pub(crate) fn output_preview_set(&mut self, a: &Args) -> Result<Value> {
        let doc = self.doc(a)?;
        let id = doc.id;
        let mut s = if a.opt_bool("reset")?.unwrap_or(false) { OutputPreview::default() } else { doc.output_preview().clone() };
        match a.opt_str("simulation_profile")? {
            Some("output_intent") => s.profile = SimulationProfile::OutputIntent,
            Some("working") => s.profile = SimulationProfile::Working,
            Some("") => return Err(bad("simulation_profile must not be empty")),
            Some(id) => s.profile = self.session.simulation_profile(id).map_err(|e| bad(e.to_string()))?,
            None => {}
        }
        if let Some(v) = a.opt_bool("simulate_overprint")? {
            s.simulate_overprint = v;
        }
        let black = a.opt_bool("simulate_black")?;
        if let Some(v) = a.opt_bool("simulate_paper")? {
            s.simulate_paper = v;
            if v && black.is_none() {
                s.simulate_black = true;
            }
        }
        if let Some(v) = black {
            s.simulate_black = v;
        }
        if let Some(v) = a.opt_str("show")? {
            s.show = Show::from_id(v)
                .ok_or_else(|| bad(format!("unknown show {v:?}; use one of {}", Show::ALL.iter().map(|s| s.id()).collect::<Vec<_>>().join(", "))))?;
        }
        if a.opt_bool("show_all_inks")?.unwrap_or(false) {
            s.hidden_inks.clear();
        }
        let known = if a.get("hide_inks").is_some() { self.preview_ink_names(a)? } else { None };
        if a.get("hide_inks").is_some() {
            let names = a.strs("hide_inks")?;
            for name in names {
                if name.is_empty() || name.len() > MAX_INK_NAME {
                    return Err(bad(format!("ink names are 1 to {MAX_INK_NAME} bytes ({name:?})")));
                }
                if let Some(known) = &known
                    && !known.iter().any(|k| k == name)
                {
                    return Err(bad(format!("the document has no ink {name:?}; its inks are {}", known.join(", "))));
                }
                s.hidden_inks.insert(name.to_string());
            }
            if s.hidden_inks.len() > MAX_HIDDEN_INKS {
                return Err(bad(format!("at most {MAX_HIDDEN_INKS} inks can be hidden")));
            }
        }
        if a.get("show_inks").is_some() {
            for name in a.strs("show_inks")? {
                s.hidden_inks.remove(name);
            }
        }
        if let Some(v) = a.opt_bool("coverage_warning")? {
            s.coverage_warning = v;
        }
        if let Some(v) = a.opt_num("coverage_limit")? {
            if !(MIN_LIMIT..=MAX_LIMIT).contains(&v) {
                return Err(bad(format!("coverage_limit must be between {MIN_LIMIT} and {MAX_LIMIT} percent")));
            }
            s.coverage_limit = v;
        }
        if let Some(c) = a.opt_str("warning_color")? {
            s.warning_colour = parse_colour(c).ok_or_else(|| bad(format!("warning_color must be #RRGGBB, not {c:?}")))?;
        }
        s.validate().map_err(bad)?;
        self.session.set_output_preview(id, s).map_err(failed)?;
        self.output_preview_state(a)
    }

    /// Render Output Preview for a page as PNG, with ink and coverage statistics.
    pub(crate) fn output_preview_render(&self, a: &Args) -> Result<Vec<Content>> {
        let page = self.page(a)?;
        let dpi = a.opt_num("dpi")?.unwrap_or(DEFAULT_DPI);
        if !(MIN_DPI..=MAX_DPI).contains(&dpi) {
            return Err(bad(format!("dpi must be between {MIN_DPI} and {MAX_DPI}")));
        }
        let point = match (a.opt_num("x")?, a.opt_num("y")?) {
            (Some(x), Some(y)) => Some([x, y]),
            (None, None) => None,
            _ => return Err(bad("give both x and y to read the inks at a point")),
        };
        let path = a.opt_str("path")?;
        if path.is_some_and(str::is_empty) {
            return Err(bad("path must not be empty"));
        }
        let doc = self.doc(a)?;
        let info = doc.info.pages.get(page);
        let rotation = info.map_or(0, |p| p.rotation);
        // Check the point before the (expensive) render.
        let user = match point {
            Some([x, y]) => {
                let (w, h) = info.map_or((0.0, 0.0), |p| (f64::from(p.width), f64::from(p.height)));
                if !(0.0..=w).contains(&x) || !(0.0..=h).contains(&y) {
                    return Err(bad(format!("the point ({x}, {y}) is outside page {} ({w} × {h} pt, origin top-left)", page + 1)));
                }
                Some(info.map_or([x, y], |p| {
                    let [ux, uy] = p.view_to_user(x as f32, y as f32);
                    [f64::from(ux), f64::from(uy)]
                }))
            }
            None => None,
        };
        let (preview, simulation) = doc.render_output_preview(page, dpi).map_err(failed)?;
        let (data, w, h) = crate::separations::rotate(&preview.rgb, preview.width, preview.height, 3, rotation);
        let bytes = crate::separations::encode(&data, w, h, png::ColorType::Rgb)?;
        drop(data);
        let mut image = None;
        let written = match path {
            Some(p) => Some(self.write_output(p, &bytes)?.display().to_string()),
            None => {
                let (Ok(width), Ok(height)) = (u32::try_from(w), u32::try_from(h)) else { return Err(failed("the preview is too large")) };
                image = Some(Content::Png { data: bytes, width, height });
                None
            }
        };
        let r = &preview.raster;
        let sample = match user {
            Some(u) => {
                let s = pdfcraft_engine::pdfa::separations::sample(r, u[0], u[1])
                    .ok_or_else(|| bad(format!("the point is outside page {}'s crop box", page + 1)))?;
                let cell = s.cell.map(|v| v as f32);
                json!({
                    "point": point,
                    "inks": s.inks.iter().map(|(name, v)| json!({ "name": name, "percent": v })).collect::<Vec<_>>(),
                    "total": s.total,
                    "cell": info.map_or(cell, |p| crate::comments::rect_to_view(p, cell)),
                    "pdf_cell": s.cell,
                })
            }
            None => Value::Null,
        };
        let c = &preview.coverage;
        let inks: Vec<Value> = preview
            .inks
            .iter()
            .map(|i| {
                let mut v = crate::separations::ink_json(&i.info);
                v["shown"] = json!(i.shown);
                v["max"] = json!(i.max);
                v["inked_percent"] = json!(i.inked_percent);
                v["mean_percent"] = json!(i.mean);
                v
            })
            .collect();
        let settings = doc.output_preview();
        let mut out = vec![Content::Json(json!({
            "doc": doc.id.0,
            "page": page + 1,
            "dpi": (r.dpi * 100.0).round() / 100.0,
            "width": w,
            "height": h,
            "rotation": rotation,
            "path": written,
            "settings": settings_json(settings),
            "simulation": simulation_json(&simulation),
            "simulated": { "overprint": settings.simulate_overprint, "paper": preview.simulated.paper, "black_ink": preview.simulated.black },
            "inks": inks,
            "coverage": {
                "max": c.max,
                "max_at": c.max_at.map(|p| crate::separations::view_point(info, p)),
                "pdf_max_at": c.max_at,
                "limit": c.limit,
                "warning": c.warning,
                "over_limit": c.over_cells > 0,
                "over_cells": c.over_cells,
                "over_percent": c.over_percent,
                "over_area": c.over_area,
                "warned_pixels": c.warned_pixels,
            },
            "sample": sample,
            "hidden_objects": r.hidden_paints,
            "conversion": r.conversion,
            "converted_paints": r.converted_paints,
            "unmeasured": r.unmeasured,
            "approximated": r.approximated,
            "truncated": r.truncated,
            "ink_manager": crate::ink_manager::settings_json(doc.ink_manager()),
            "routed": r.routes,
            "method": pdfcraft_engine::pdfa::coverage::METHOD,
        }))];
        out.extend(image);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warning_colours_parse_strictly() {
        assert_eq!(parse_colour("#00ff7F"), Some([0, 255, 127]));
        for bad in ["00FF00", "#00FF0", "#GG0000", "#00FF00FF", "#ÿÿÿ", ""] {
            assert_eq!(parse_colour(bad), None, "{bad}");
        }
        assert_eq!(colour_hex([1, 2, 255]), "#0102FF");
    }
}
