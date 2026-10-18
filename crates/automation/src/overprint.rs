//! Overprint Preview: the preference (`print_overprint_preview`), its effect on `page_render`
//! and `render_preview` (their `overprint_preview` option), and the objects of a page that
//! overprint (`print_overprint_objects`).

use pdfcraft_engine::DocId;
use pdfcraft_engine::preflight::{OVERPRINT_DEFAULT_DPI, OVERPRINT_DEFAULT_MAX_OBJECTS, OVERPRINT_MAX_OBJECTS, OverprintPreview};
use pdfcraft_render::{RenderRequest, RequestKind};
use serde_json::{Value, json};

use crate::{Args, Automation, Content, Result, ToolError, encode_png, failed};

fn bad(m: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs(m.into())
}

fn mode_list() -> Vec<Value> {
    OverprintPreview::ALL.iter().map(|m| json!({ "id": m.id(), "label": m.label() })).collect()
}

/// A mode named by `key`, when given.
pub(crate) fn mode_arg(a: &Args, key: &str) -> Result<Option<OverprintPreview>> {
    let Some(id) = a.opt_str(key)? else { return Ok(None) };
    OverprintPreview::from_id(id).map(Some).ok_or_else(|| {
        let ids: Vec<&str> = OverprintPreview::ALL.iter().map(|m| m.id()).collect();
        bad(format!("{key} must be one of {}, not {id:?}", ids.join(", ")))
    })
}

impl Automation {
    /// Render 0-based `page` of `id` at `scale` as PNG, simulating overprint as `mode` says
    /// (`None`: the session's preference). A simulation the caller asked for that fails is an
    /// error; one that only the preference asked for leaves the normal render.
    pub(crate) fn render_png(&mut self, id: DocId, page: usize, scale: f32, mode: Option<OverprintPreview>) -> Result<Content> {
        let out = self.renderer(id)?.render(RenderRequest { page, kind: RequestKind::Pixels, tile: None, scale, tag: 0 });
        if let Some(e) = out.error {
            return Err(failed(format!("page {} could not be rendered: {e}", page + 1)));
        }
        let explicit = mode.is_some();
        let mode = mode.unwrap_or_else(|| self.session.overprint_preview());
        let doc = self.session.get(id).ok_or_else(|| failed("no such document"))?;
        let applies = match doc.overprint_preview_applies(mode) {
            Ok(applies) => applies,
            Err(e) if explicit && mode != OverprintPreview::Never => return Err(failed(format!("overprint preview: {e}"))),
            Err(_) => false,
        };
        if !applies {
            let data = encode_png(out.width, out.height, &out.rgba)?;
            return Ok(Content::Png { data, width: out.width, height: out.height });
        }
        let mut rgba = out.rgba.to_vec();
        let (w, h) = (out.width as usize, out.height as usize);
        match doc.simulate_overprint(page, &mut rgba, w, h) {
            Ok(_) => {}
            Err(e) if explicit => return Err(failed(e)),
            Err(_) => rgba = out.rgba.to_vec(),
        }
        let data = encode_png(out.width, out.height, &rgba)?;
        Ok(Content::Png { data, width: out.width, height: out.height })
    }

    /// Preferences ▸ Page Display ▸ Use Overprint Preview: read or set it.
    pub(crate) fn overprint_preview(&mut self, a: &Args) -> Result<Value> {
        if let Some(mode) = mode_arg(a, "mode")? {
            self.session.set_overprint_preview(mode);
        }
        let mode = self.session.overprint_preview();
        let mut out = json!({ "mode": mode.id(), "label": mode.label(), "modes": mode_list() });
        if a.opt_int("doc")?.is_some() {
            let doc = self.doc(a)?;
            out["doc"] = json!(doc.id.0);
            out["pdfx"] = json!(doc.is_pdfx().map_err(failed)?);
            out["applies"] = json!(doc.overprint_preview_applies(mode).map_err(failed)?);
        }
        Ok(out)
    }

    /// The objects of a page painted with overprint on.
    pub(crate) fn overprint_objects(&self, a: &Args) -> Result<Value> {
        let doc = self.doc(a)?;
        let page = self.page(a)?;
        let dpi = a.opt_num("dpi")?.unwrap_or(OVERPRINT_DEFAULT_DPI);
        let (lo, hi) = (pdfcraft_engine::pdfa::coverage::MIN_DPI, pdfcraft_engine::pdfa::coverage::MAX_DPI);
        if !(lo..=hi).contains(&dpi) {
            return Err(bad(format!("dpi must be between {lo} and {hi}")));
        }
        let max = match a.opt_int("max_objects")? {
            None => OVERPRINT_DEFAULT_MAX_OBJECTS,
            Some(n) => usize::try_from(n)
                .ok()
                .filter(|n| (1..=OVERPRINT_MAX_OBJECTS).contains(n))
                .ok_or_else(|| bad(format!("max_objects must be 1 to {OVERPRINT_MAX_OBJECTS}, not {n}")))?,
        };
        let list = doc.overprinting_objects(page, dpi, max).map_err(failed)?;
        let info = doc.info.pages.get(page);
        let rect = |r: [f64; 4]| -> Value {
            let r32 = r.map(|v| v as f32);
            json!(info.map_or(r32, |p| crate::comments::rect_to_view(p, r32)))
        };
        let finding = |f: &pdfcraft_engine::preflight::ColourFinding| {
            json!({
                "kind": f.kind.id(),
                "severity": f.severity.id(),
                "object": f.object.map(|o| o.num),
                "message": f.message,
                "paint": f.paint.map(|p| p.id()),
                "colour": f.colour.as_ref().map(|c| json!({ "space": c.space, "components": c.components, "cmyk": c.cmyk, "via_alternate": c.via_alternate })),
                "overprint_mode": f.overprint_mode,
                "rect": f.bbox.map(rect),
                "pdf_rect": f.bbox,
                "occurrences": f.occurrences,
            })
        };
        let objects: Vec<Value> = list
            .objects
            .iter()
            .map(|o| {
                let mut v = finding(&o.finding);
                v["changes_appearance"] = json!(o.changes_appearance());
                v["changed_cells"] = json!(o.changed_cells);
                v
            })
            .collect();
        let d = &list.difference;
        let mode = self.session.overprint_preview();
        Ok(json!({
            "doc": doc.id.0,
            "page": page + 1,
            "objects": objects,
            "object_count": objects.len(),
            "objects_truncated": list.objects_truncated,
            "visible_objects": list.objects.iter().filter(|o| o.changes_appearance() == Some(true)).count(),
            "states": list.states.iter().map(finding).collect::<Vec<_>>(),
            "overprint_paints": d.overprint_paints,
            "changed_percent": d.changed_percent(),
            "dpi": (d.dpi * 100.0).round() / 100.0,
            "preference": mode.id(),
            "preview_applies": doc.overprint_preview_applies(mode).map_err(failed)?,
            "conversion": d.conversion,
            "unmeasured": d.unmeasured,
            "approximated": d.approximated,
            "unresolved": list.unresolved,
            "truncated": d.truncated,
            "complete": list.limits.is_empty() && !d.truncated,
            "limits": list.limits,
        }))
    }
}
