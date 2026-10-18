//! Scan & OCR ▸ Enhance ▸ Camera Image: find a photographed document's page in the photo a page
//! draws, correct its perspective to an upright rectangle and even out its lighting, replacing
//! the page ([`Edit::CorrectCameraImages`]). The pixel work is [`pdfcraft_optimize::camera`].

use pdfcraft_optimize::camera;
pub use pdfcraft_optimize::camera::{EnhanceStats, MIN_CONFIDENCE, Plan, Quad};

use crate::{DocId, Edit, EditError, Session};

/// One page to correct, resolved: what [`Edit::CorrectCameraImages`] applies.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraFix {
    /// 0-based.
    pub page: usize,
    /// The page's outline in the photo's pixels, ordered top-left, top-right, bottom-right,
    /// bottom-left as displayed.
    pub quad: Quad,
    pub enhance: bool,
    /// The new page's longer side (points): the old page's.
    pub long_side: f64,
}

/// How to correct camera images.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraOptions {
    /// The page's corners in points from the top-left of the displayed page (any order),
    /// instead of detecting them.
    pub corners: Option<Quad>,
    /// Even out lighting, stretch contrast and whiten the paper (default true).
    pub enhance: bool,
    /// Leave pages alone where no outline is found (otherwise they are only enhanced).
    pub only_found: bool,
}

impl Default for CameraOptions {
    fn default() -> Self {
        CameraOptions { corners: None, enhance: true, only_found: false }
    }
}

/// What was found (and done) on one page.
#[derive(Clone, Debug, PartialEq)]
pub struct CameraPage {
    /// 0-based.
    pub page: usize,
    /// Why the page was left alone.
    pub skipped: Option<String>,
    /// Whether a page outline was found (or given).
    pub found: bool,
    /// Whether the corners were given.
    pub manual: bool,
    /// How well the photo's edges support the outline (0–1; 1 for given corners).
    pub confidence: f64,
    /// The outline in points from the top-left of the displayed page, ordered top-left,
    /// top-right, bottom-right, bottom-left.
    pub corners: Quad,
    /// The same in the photo's pixels.
    pub image_corners: Quad,
    /// The photo's size in pixels.
    pub image_size: [u32; 2],
    /// The corrected image's size and estimated aspect ratio.
    pub plan: Option<Plan>,
    /// The new page size in points.
    pub page_size: Option<[f64; 2]>,
}

impl CameraPage {
    fn skipped(page: usize, why: impl Into<String>) -> CameraPage {
        CameraPage {
            page,
            skipped: Some(why.into()),
            found: false,
            manual: false,
            confidence: 0.0,
            corners: [[0.0; 2]; 4],
            image_corners: [[0.0; 2]; 4],
            image_size: [0, 0],
            plan: None,
            page_size: None,
        }
    }
}

/// `Edit::CorrectCameraImages`: correct each page in turn.
pub(crate) fn apply(doc: &mut pdfcraft_cos::Document, fixes: &[CameraFix]) -> Result<(), EditError> {
    for f in fixes {
        let settings = pdfcraft_optimize::CameraSettings { quad: f.quad, enhance: f.enhance, long_side: f.long_side };
        pdfcraft_optimize::correct_camera_page(doc, f.page, &settings).map_err(|e| EditError::Optimize(e.to_string()))?;
    }
    Ok(())
}

impl Session {
    /// Scan & OCR ▸ Enhance ▸ Camera Image on `pages` (0-based; empty = all) of document `id`:
    /// per page, the outline of the photographed page is detected in the largest image it
    /// draws (or taken from `options.corners`), and the page is replaced by the page
    /// photographed, straightened to its estimated aspect ratio, as one undoable step. Pages
    /// without a usable image are reported as skipped; where no outline is found the whole
    /// image is only enhanced (or, with `only_found` or without `enhance`, skipped). With
    /// `dry_run` nothing changes. Returns per page what was found.
    pub fn correct_camera_images(
        &mut self,
        id: DocId,
        pages: &[usize],
        options: &CameraOptions,
        dry_run: bool,
    ) -> Result<Vec<CameraPage>, EditError> {
        let doc = self.get(id).ok_or_else(|| EditError::Invalid("no such document".into()))?;
        let all = doc.info.pages.len();
        let mut unique = std::collections::BTreeSet::new();
        if pages.iter().any(|p| *p >= all || !unique.insert(*p)) {
            return Err(EditError::Invalid(format!("camera image pages must be unique page indexes below {all}")));
        }
        if let Some(c) = &options.corners
            && c.iter().flatten().any(|v| !v.is_finite() || v.abs() > 1e6)
        {
            return Err(EditError::Invalid("corners must be finite numbers of points".into()));
        }
        if !dry_run {
            if let Some(why) = &doc.read_only_reason {
                return Err(EditError::ReadOnly(why.clone()));
            }
            if !doc.allows_modification() {
                return Err(EditError::NotPermitted("changes to the document"));
            }
        }
        let cos = &doc.editor.as_ref().ok_or_else(|| EditError::ReadOnly(doc.read_only_reason.clone().unwrap_or_default()))?.cos;
        let pages: Vec<usize> = if pages.is_empty() { (0..all).collect() } else { pages.to_vec() };
        let mut out = Vec::new();
        let mut fixes = Vec::new();
        for page in pages {
            let Some(info) = doc.info.pages.get(page) else { continue };
            let found = crate::guard(|| pdfcraft_optimize::camera_image(cos, page))
                .map_err(|m| EditError::Invalid(format!("reading page {}'s image failed unexpectedly ({m})", page + 1)))?;
            let (img, px) = match found {
                Ok(f) => f,
                Err(e) => {
                    out.push(CameraPage::skipped(page, e.to_string()));
                    continue;
                }
            };
            let to_view = |p: [f64; 2]| {
                let u = img.to_user(p);
                let v = info.user_to_view(u[0] as f32, u[1] as f32);
                [f64::from(v[0]), f64::from(v[1])]
            };
            let (quad, confidence, detected, manual) = match &options.corners {
                Some(c) => {
                    let mut q = [[0.0; 2]; 4];
                    for (o, p) in q.iter_mut().zip(c) {
                        let u = info.view_to_user(p[0] as f32, p[1] as f32);
                        *o = img
                            .to_image([f64::from(u[0]), f64::from(u[1])])
                            .ok_or_else(|| EditError::Invalid(format!("page {}'s image is drawn without area", page + 1)))?;
                    }
                    (q, 1.0, true, true)
                }
                None => {
                    let d = crate::guard(|| camera::detect(&px, img.width, img.height, img.channels))
                        .map_err(|m| EditError::Invalid(format!("finding the page outline failed unexpectedly ({m})")))?
                        .map_err(|e| EditError::Invalid(e.to_string()))?;
                    (d.quad, d.confidence, d.found, false)
                }
            };
            drop(px);
            // Order the corners as displayed, keeping each with its pixel.
            let views = quad.map(to_view);
            let ordered = camera::order_quad(views);
            let mut q = quad;
            for (k, v) in ordered.iter().enumerate() {
                if let (Some(i), Some(slot)) = (views.iter().position(|w| w == v), q.get_mut(k)) {
                    *slot = quad.get(i).copied().unwrap_or(*slot);
                }
            }
            let mut report = CameraPage {
                page,
                skipped: None,
                found: detected,
                manual,
                confidence: (confidence * 1000.0).round() / 1000.0,
                corners: ordered,
                image_corners: q,
                image_size: [img.width, img.height],
                plan: None,
                page_size: None,
            };
            let plan = match camera::plan(&q, img.width, img.height) {
                Ok(p) => p,
                Err(e) if manual => return Err(EditError::Invalid(format!("page {}: {e}", page + 1))),
                Err(e) => {
                    report.skipped = Some(e.to_string());
                    out.push(report);
                    continue;
                }
            };
            if !detected && (options.only_found || !options.enhance) {
                report.skipped = Some(format!(
                    "no page outline was found in the photo (confidence {:.2}, at least {MIN_CONFIDENCE} needed); give its corners",
                    report.confidence
                ));
                out.push(report);
                continue;
            }
            let long_side = f64::from(info.width.max(info.height)).clamp(3.0, 14_400.0);
            let (pw, ph) = pdfcraft_optimize::page_size(plan.aspect, long_side).map_err(|e| EditError::Invalid(e.to_string()))?;
            report.plan = Some(plan);
            report.page_size = Some([(pw * 1000.0).round() / 1000.0, (ph * 1000.0).round() / 1000.0]);
            fixes.push(CameraFix { page, quad: q, enhance: options.enhance, long_side });
            out.push(report);
        }
        if !dry_run && !fixes.is_empty() {
            self.apply(id, Edit::CorrectCameraImages { fixes })?;
        }
        Ok(out)
    }
}
