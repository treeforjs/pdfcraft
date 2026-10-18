//! Output Preview's Object Inspector (`print_object_inspector`): every object under a point of
//! a page, topmost first, and the ink of every plate there.

use pdfcraft_engine::preflight::{Inspected, ObjectColour};
use pdfcraft_render::PageInfo;
use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError, failed};

fn bad(m: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs(m.into())
}

const DEFAULT_DPI: f64 = 72.0;
const MIN_DPI: f64 = pdfcraft_engine::pdfa::coverage::MIN_DPI;
const MAX_DPI: f64 = pdfcraft_engine::pdfa::coverage::MAX_DPI;

fn colour_json(c: &ObjectColour) -> Value {
    json!({
        "space": c.space,
        "components": c.components,
        "inks": c.inks.iter().map(|(name, v)| json!({ "name": name, "percent": v })).collect::<Vec<_>>(),
        "converted": c.converted,
        "measured": c.measured,
        "pattern": c.pattern,
    })
}

fn object_json(o: &Inspected, info: Option<&PageInfo>) -> Value {
    let rect = info.map_or_else(|| json!(o.bbox), |p| json!(crate::comments::rect_to_view(p, o.bbox.map(|v| v as f32))));
    let t = &o.transparency;
    let mut v = json!({
        "type": o.kind.as_str(),
        "order": o.order,
        "rect": rect,
        "pdf_rect": o.bbox,
        "painted": o.painted,
        "fill": o.fill.as_ref().map(colour_json),
        "stroke": o.stroke.as_ref().map(colour_json),
        "overprint": { "fill": o.overprint.fill, "stroke": o.overprint.stroke, "mode": o.overprint.mode, "op": o.overprint.fill, "OP": o.overprint.stroke, "OPM": o.overprint.mode },
        "transparency": {
            "transparent": t.is_transparent(),
            "blend_mode": t.blend_mode,
            "fill_alpha": t.fill_alpha,
            "stroke_alpha": t.stroke_alpha,
            "soft_mask": t.soft_mask,
            "alpha_is_shape": t.alpha_is_shape,
        },
        "rendering_intent": o.rendering_intent,
        "line_width": o.line_width,
        "fill_rule": o.fill_rule,
        "forms": o.forms,
    });
    if let Some(text) = &o.text {
        v["text"] = json!({
            "font": text.font,
            "resource": text.resource,
            "font_type": text.font_type,
            "embedded": text.embedded,
            "subset": text.subset,
            "size": text.size,
            "effective_size": text.effective_size,
            "render_mode": text.render_mode,
            "invisible": text.invisible,
        });
    }
    if let Some(im) = &o.image {
        v["image"] = json!({
            "name": im.name,
            "inline": im.name.is_none(),
            "width": im.width,
            "height": im.height,
            "bits_per_component": im.bits_per_component,
            "colour_space": im.colour_space,
            "stencil": im.stencil,
            "filters": im.filters,
            "ppi": im.ppi,
            "effective_ppi": im.ppi[0].min(im.ppi[1]),
            "mask": im.mask,
            "interpolate": im.interpolate,
            "intent": im.intent,
        });
    }
    if let Some(sh) = &o.shading {
        v["shading"] = json!({ "name": sh.name, "shading_type": sh.shading_type, "description": sh.description, "colour_space": sh.colour_space });
    }
    if let Some(f) = &o.form {
        v["form"] = json!({ "name": f.name, "group": f.group });
    }
    if let Some(a) = &o.annotation {
        v["annotation"] =
            json!({ "subtype": a.subtype, "name": a.name, "hidden": a.hidden, "printed": a.printed, "has_appearance": a.has_appearance });
    }
    v
}

impl Automation {
    /// Every object at a point, topmost first, and the inks there.
    pub(crate) fn object_inspector(&self, a: &Args) -> Result<Value> {
        let x = a.opt_num("x")?.ok_or_else(|| bad("missing argument x"))?;
        let y = a.opt_num("y")?.ok_or_else(|| bad("missing argument y"))?;
        if !(x.is_finite() && y.is_finite()) {
            return Err(bad("x and y must be finite numbers"));
        }
        let units = a.opt_str("units")?.unwrap_or("view");
        if !matches!(units, "view" | "pdf") {
            return Err(bad(format!("units must be view (points from the top-left of the displayed page) or pdf (user space), not {units:?}")));
        }
        let dpi = a.opt_num("dpi")?.unwrap_or(DEFAULT_DPI);
        if !(MIN_DPI..=MAX_DPI).contains(&dpi) {
            return Err(bad(format!("dpi must be between {MIN_DPI} and {MAX_DPI}")));
        }
        let overprint = a.opt_bool("overprint")?.unwrap_or(true);
        let page = self.page(a)?;
        let doc = self.doc(a)?;
        let info = doc.info.pages.get(page);
        let user = if units == "view" {
            let (w, h) = info.map_or((0.0, 0.0), |p| (f64::from(p.width), f64::from(p.height)));
            if !(0.0..=w).contains(&x) || !(0.0..=h).contains(&y) {
                return Err(bad(format!("the point ({x}, {y}) is outside page {} ({w} × {h} pt, origin top-left)", page + 1)));
            }
            info.map_or([x, y], |p| {
                let [ux, uy] = p.view_to_user(x as f32, y as f32);
                [f64::from(ux), f64::from(uy)]
            })
        } else {
            [x, y]
        };
        let r = doc.inspect_objects(page, user[0], user[1], dpi, overprint).map_err(failed)?;
        let seps = &r.raster;
        let rect = |b: [f64; 4]| -> Value { info.map_or_else(|| json!(b), |p| json!(crate::comments::rect_to_view(p, b.map(|v| v as f32)))) };
        let inks = r.sample.as_ref().map(|s| {
            json!({
                "inks": s.inks.iter().map(|(name, v)| json!({ "name": name, "percent": v })).collect::<Vec<_>>(),
                "total": s.total,
                "cell": rect(s.cell),
                "pdf_cell": s.cell,
            })
        });
        let round = |v: f64| (v * 100.0).round() / 100.0;
        Ok(json!({
            "doc": doc.id.0,
            "page": page + 1,
            "point": crate::separations::view_point(info, user),
            "pdf_point": [round(user[0]), round(user[1])],
            "objects": r.objects.iter().map(|o| object_json(o, info)).collect::<Vec<_>>(),
            "count": r.objects.len(),
            "objects_truncated": r.objects_truncated,
            "separations": inks,
            "dpi": round(seps.dpi),
            "overprint_simulated": seps.overprint,
            "conversion": seps.conversion,
            "unmeasured": seps.unmeasured,
            "approximated": seps.approximated,
            "truncated": seps.truncated,
            "ink_manager": crate::ink_manager::settings_json(doc.ink_manager()),
            "routed": seps.routes,
            "method": "Objects come from PdfCraft's ink-separation content interpreter: fills are hit-tested with their winding rule, strokes within half the line width (at least 0.5 pt), images by their unit square, shadings by their clip; clipping paths count as bounding boxes, text as the box of each shown string, curves are flattened, optional content is not hidden, annotations are listed by their rectangle.",
        }))
    }
}
