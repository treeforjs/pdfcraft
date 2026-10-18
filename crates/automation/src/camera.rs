//! Scan & OCR ▸ Enhance ▸ Camera Image (`scan_camera_image`), and Recognize text's `camera`
//! option.
use crate::{Args, Automation, Result, ToolError, failed};
use pdfcraft_engine::EditError;
use pdfcraft_engine::camera::{CameraOptions, CameraPage, Quad};
use serde_json::{Value, json};

fn r3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn quad_json(q: &Quad) -> Value {
    json!(q.iter().map(|p| [r3(p[0]), r3(p[1])]).collect::<Vec<_>>())
}

pub(crate) fn page_json(p: &CameraPage) -> Value {
    let mut v = json!({ "page": p.page + 1 });
    if p.image_size != [0, 0] {
        v["found"] = json!(p.found);
        v["manual"] = json!(p.manual);
        v["confidence"] = json!(p.confidence);
        v["corners"] = quad_json(&p.corners);
        v["image_corners"] = quad_json(&p.image_corners);
        v["image_size"] = json!(p.image_size);
    }
    if let Some(plan) = &p.plan {
        v["aspect"] = json!(r3(plan.aspect));
        v["output_size"] = json!([plan.width, plan.height]);
    }
    if let Some(s) = &p.page_size {
        v["page_size"] = json!(s);
    }
    if let Some(why) = &p.skipped {
        v["skipped"] = json!(why);
    }
    v
}

/// `corners`: four [x, y] points from the top-left of the displayed page.
fn corners(a: &Args) -> Result<Option<Quad>> {
    let Some(v) = a.get("corners") else { return Ok(None) };
    let wrong = || ToolError::InvalidArgs("corners must be four [x, y] points (numbers, points from the top-left of the displayed page)".into());
    let list = v.as_array().filter(|l| l.len() == 4).ok_or_else(wrong)?;
    let mut q = [[0.0; 2]; 4];
    for (o, p) in q.iter_mut().zip(list) {
        let xy = p.as_array().filter(|xy| xy.len() == 2).ok_or_else(wrong)?;
        for (c, n) in o.iter_mut().zip(xy) {
            *c = n.as_f64().filter(|f| f.is_finite()).ok_or_else(wrong)?;
        }
    }
    Ok(Some(q))
}

fn edit_error(e: EditError) -> ToolError {
    match e {
        EditError::Invalid(m) => ToolError::InvalidArgs(m),
        e => failed(e),
    }
}

impl Automation {
    pub(crate) fn scan_camera_image(&mut self, a: &Args) -> Result<Value> {
        let options = CameraOptions { corners: corners(a)?, enhance: a.opt_bool("enhance")?.unwrap_or(true), only_found: false };
        let dry_run = a.opt_bool("dry_run")?.unwrap_or(false);
        let pages = if a.opt_ints("pages")?.is_some() { self.pages(a, "pages")? } else { Vec::new() };
        let id = self.doc(a)?.id;
        let found = self.session.correct_camera_images(id, &pages, &options, dry_run).map_err(edit_error)?;
        let changed = found.iter().filter(|p| p.skipped.is_none()).count();
        Ok(json!({
            "pages": found.iter().map(page_json).collect::<Vec<_>>(),
            "changed": if dry_run { 0 } else { changed },
            "would_change": changed,
            "dry_run": dry_run,
            "undoable": !dry_run && changed > 0,
        }))
    }

    /// Recognize text with `camera` true: correct the pages where a photographed page is found
    /// first; the correction and the recognized text undo as one step. When recognition fails
    /// the correction is undone too.
    pub(crate) fn camera_then_recognize(&mut self, a: &Args, recognize: impl FnOnce(&mut Self) -> Result<Value>) -> Result<Value> {
        let pages = if a.opt_ints("pages")?.is_some() { self.pages(a, "pages")? } else { Vec::new() };
        let id = self.doc(a)?.id;
        let before = self.doc(a)?.edit_generation();
        let options = CameraOptions { only_found: true, ..Default::default() };
        let found = self.session.correct_camera_images(id, &pages, &options, false).map_err(edit_error)?;
        let corrected = found.iter().any(|p| p.skipped.is_none());
        let result = recognize(self);
        let mut out = match result {
            Ok(v) => v,
            Err(e) => {
                if corrected {
                    let _ = self.session.undo(id);
                }
                return Err(e);
            }
        };
        if corrected {
            self.session.group_undo(id, before, "Recognize text").map_err(failed)?;
        }
        out["camera"] = json!(found.iter().map(page_json).collect::<Vec<_>>());
        Ok(out)
    }
}
