//! The PDF side of camera-image correction ([`crate::camera`]): find the photo a page draws,
//! and replace the page by its corrected version.

use std::collections::HashSet;

use pdfcraft_content::Matrix;
use pdfcraft_cos::{Dict, Document, ObjRef, Object, Stream};

use crate::camera::{self, CameraError, EnhanceStats, Plan, Quad};
use crate::images::{MAX_PIXELS, components, content_bytes, image_stream, is_dct, page_resources, pixels};
use crate::{Compression, OptimizeError};

/// The largest image a page draws: the camera photo.
#[derive(Clone, Debug, PartialEq)]
pub struct CameraImage {
    pub image: ObjRef,
    pub width: u32,
    pub height: u32,
    /// 1 (gray) or 3 (RGB).
    pub channels: usize,
    /// The image's unit square → page user space (the CTM where it is drawn).
    pub matrix: [f64; 6],
    /// Whether it is a JPEG (the corrected image is too).
    pub jpeg: bool,
}

impl CameraImage {
    /// Image pixel `p` (continuous, y down from the top row) → user space.
    pub fn to_user(&self, p: [f64; 2]) -> [f64; 2] {
        let (u, v) = (p[0] / f64::from(self.width.max(1)), 1.0 - p[1] / f64::from(self.height.max(1)));
        let (x, y) = Matrix(self.matrix).apply(u, v);
        [x, y]
    }

    /// User space → image pixel (`None` when the image is drawn degenerate).
    pub fn to_image(&self, p: [f64; 2]) -> Option<[f64; 2]> {
        let (u, v) = Matrix(self.matrix).invert()?.apply(p[0], p[1]);
        let out = [u * f64::from(self.width), (1.0 - v) * f64::from(self.height)];
        (out[0].is_finite() && out[1].is_finite()).then_some(out)
    }
}

/// Every image XObject drawn (directly or through forms) with its CTM.
fn draws(doc: &Document, data: &[u8], resources: &Dict, ctm: Matrix, depth: usize, seen: &mut HashSet<ObjRef>, out: &mut Vec<(ObjRef, Matrix)>) {
    if depth > 12 || out.len() > 4096 {
        return;
    }
    let xobjects = resources.get(b"XObject").map(|x| doc.resolve(x)).and_then(|x| x.as_dict().cloned()).unwrap_or_default();
    let mut stack = vec![ctm];
    for op in pdfcraft_content::parse(data).ops {
        let top = stack.last().copied().unwrap_or(ctm);
        match op.op.as_slice() {
            b"q" if stack.len() < 256 => stack.push(top),
            b"Q" if stack.len() > 1 => {
                stack.pop();
            }
            b"cm" => {
                if let Some(m) = Matrix::from_operands(&op.operands)
                    && let Some(t) = stack.last_mut()
                {
                    *t = m.then(&top);
                }
            }
            b"Do" => {
                let Some(name) = op.name(0) else { continue };
                let Some(r) = xobjects.get(name).and_then(Object::as_ref) else { continue };
                let obj = doc.get(r);
                let Object::Stream(s) = &*obj else { continue };
                match s.dict.name(b"Subtype") {
                    Some(b"Image") => out.push((r, top)),
                    Some(b"Form") if seen.insert(r) => {
                        let m = s.dict.get(b"Matrix").map(|m| doc.resolve(m)).and_then(|m| m.as_array().map(|a| Matrix::from_operands(a))).flatten();
                        let res =
                            s.dict.get(b"Resources").map(|r| doc.resolve(r)).and_then(|r| r.as_dict().cloned()).unwrap_or_else(|| resources.clone());
                        let inner = m.unwrap_or(Matrix([1.0, 0.0, 0.0, 1.0, 0.0, 0.0])).then(&top);
                        draws(doc, &s.decoded().unwrap_or_default(), &res, inner, depth + 1, seen, out);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

fn invalid(m: impl Into<String>) -> OptimizeError {
    OptimizeError::Invalid(m.into())
}

fn camera_error(e: CameraError) -> OptimizeError {
    OptimizeError::Invalid(e.to_string())
}

fn page_ref(doc: &Document, page: usize) -> Result<ObjRef, OptimizeError> {
    let all = pdfcraft_annot::page_refs(doc).map_err(|_| OptimizeError::NoPages)?;
    all.get(page).copied().ok_or_else(|| invalid(format!("page {} is not in the document", page.saturating_add(1))))
}

/// The photo page `page` (0-based) draws, the largest image by drawn area, and its decoded
/// 8-bit samples. Refuses pages without an image, and images PdfCraft cannot decode
/// faithfully (anything but 8-bit gray or RGB, decode arrays, colour-key masks, JPEG 2000,
/// JBIG2, more than [`camera::MAX_INPUT_PIXELS`] pixels).
pub fn camera_image(doc: &Document, page: usize) -> Result<(CameraImage, Vec<u8>), OptimizeError> {
    let r = page_ref(doc, page)?;
    let d = doc.get(r).as_dict().cloned().ok_or_else(|| invalid("the page is not a dictionary"))?;
    let res = page_resources(doc, r).unwrap_or_default();
    let mut found = Vec::new();
    if let Some(contents) = d.get(b"Contents") {
        draws(doc, &content_bytes(doc, contents), &res, Matrix([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]), 0, &mut HashSet::new(), &mut found);
    }
    let (image, m) = found
        .into_iter()
        .map(|(r, m)| {
            let [a, b, c, d, ..] = m.0;
            (r, m, (a * d - b * c).abs())
        })
        .filter(|(.., area)| area.is_finite() && *area > 0.0)
        .max_by(|x, y| x.2.total_cmp(&y.2))
        .map(|(r, m, _)| (r, m))
        .ok_or_else(|| invalid(format!("page {} draws no image to correct", page.saturating_add(1))))?;
    let obj = doc.get(image);
    let Object::Stream(s) = &*obj else { return Err(invalid("the page's image is not a stream")) };
    let unsupported = || {
        invalid(format!("page {}'s image cannot be corrected: only 8-bit gray or RGB JPEG and Flate images are supported", page.saturating_add(1)))
    };
    let sd = &s.dict;
    if sd.contains(b"Decode")
        || sd.contains(b"Mask")
        || sd.int(b"BitsPerComponent") != Some(8)
        || matches!(sd.get(b"ImageMask"), Some(Object::Bool(true)))
    {
        return Err(unsupported());
    }
    let n = sd.get(b"ColorSpace").and_then(|cs| components(doc, cs)).ok_or_else(unsupported)?;
    let jpeg = is_dct(s).ok_or_else(unsupported)?;
    let (w, h) =
        (sd.int(b"Width").and_then(|v| u32::try_from(v).ok()).unwrap_or(0), sd.int(b"Height").and_then(|v| u32::try_from(v).ok()).unwrap_or(0));
    if w == 0 || h == 0 {
        return Err(unsupported());
    }
    if u64::from(w) * u64::from(h) > MAX_PIXELS.min(camera::MAX_INPUT_PIXELS) {
        return Err(invalid(format!(
            "page {}'s image is too large to correct (at most {} pixels)",
            page.saturating_add(1),
            camera::MAX_INPUT_PIXELS
        )));
    }
    let need = (w as usize).checked_mul(h as usize).and_then(|p| p.checked_mul(n)).ok_or_else(unsupported)?;
    let px = if jpeg {
        pixels(s, n).ok_or_else(unsupported)?
    } else {
        s.decoded_complete(need.saturating_add(1 << 16)).map_err(|_| unsupported())?.get(..need).ok_or_else(unsupported)?.to_vec()
    };
    if px.len() != need {
        return Err(unsupported());
    }
    Ok((CameraImage { image, width: w, height: h, channels: n, matrix: m.0, jpeg }, px))
}

/// How to correct one page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraSettings {
    /// The page's corners in the image's pixels, ordered top-left, top-right, bottom-right,
    /// bottom-left as the page is displayed.
    pub quad: Quad,
    /// Even out lighting and stretch contrast ([`camera::enhance`]).
    pub enhance: bool,
    /// The new page's longer side, in points.
    pub long_side: f64,
}

/// What [`crate::correct_camera_page`] did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraReport {
    pub plan: Plan,
    /// The new page size, points.
    pub page_width: f64,
    pub page_height: f64,
    pub enhance: Option<EnhanceStats>,
}

/// The new page's size (points) for a corrected image of aspect `aspect` and longer side
/// `long_side` (3–14 400 points).
pub fn page_size(aspect: f64, long_side: f64) -> Result<(f64, f64), OptimizeError> {
    if !(long_side.is_finite() && (3.0..=14_400.0).contains(&long_side)) || !(aspect.is_finite() && aspect > 0.0) {
        return Err(invalid("the corrected page size must be between 3 and 14 400 points"));
    }
    let (w, h) = if aspect >= 1.0 { (long_side, long_side / aspect) } else { (long_side * aspect, long_side) };
    Ok((w.max(1.0), h.max(1.0)))
}

pub(crate) fn correct(doc: &mut Document, page: usize, settings: &CameraSettings) -> Result<CameraReport, OptimizeError> {
    let r = page_ref(doc, page)?;
    let (img, px) = camera_image(doc, page)?;
    let plan = camera::plan(&settings.quad, img.width, img.height).map_err(camera_error)?;
    let (pw, ph) = page_size(plan.aspect, settings.long_side)?;
    let mut out = camera::warp(&px, img.width, img.height, img.channels, &settings.quad, plan.width, plan.height).map_err(camera_error)?;
    drop(px);
    let enhance = if settings.enhance { Some(camera::enhance(&mut out, plan.width, plan.height, img.channels).map_err(camera_error)?) } else { None };
    let old = match &*doc.get(img.image) {
        Object::Stream(s) => s.dict.clone(),
        _ => return Err(invalid("the page's image is not a stream")),
    };
    let mut dict = Dict::new();
    for k in [&b"Type"[..], b"Subtype", b"ColorSpace", b"Intent", b"Interpolate"] {
        if let Some(v) = old.get(k) {
            dict.set(k.to_vec(), v.clone());
        }
    }
    let compression = if img.jpeg { Compression::Jpeg(90) } else { Compression::Flate };
    let stream = image_stream(&dict, &out, img.channels, plan.width, plan.height, compression, img.jpeg)
        .ok_or_else(|| invalid("the corrected image could not be encoded"))?;
    let image = doc.add(Object::Stream(stream));
    let content = format!("q {} 0 0 {} 0 0 cm /CamIm0 Do Q\n", fmt(pw), fmt(ph));
    let contents = doc.add(Object::Stream(Stream::flate(Dict::new(), content.as_bytes())));
    let mut xobjects = Dict::new();
    xobjects.set(b"CamIm0".to_vec(), Object::Ref(image));
    let mut resources = Dict::new();
    resources.set(b"XObject".to_vec(), Object::Dict(xobjects));
    doc.update_dict(r, |d| {
        for k in [&b"CropBox"[..], b"BleedBox", b"TrimBox", b"ArtBox", b"UserUnit", b"Thumb", b"Group", b"StructParents", b"PieceInfo", b"VP"] {
            d.remove(k);
        }
        d.set(b"MediaBox".to_vec(), Object::Array(vec![Object::Int(0), Object::Int(0), Object::Real(round(pw)), Object::Real(round(ph))]));
        d.set(b"Rotate".to_vec(), Object::Int(0));
        d.set(b"Resources".to_vec(), Object::Dict(resources));
        d.set(b"Contents".to_vec(), Object::Ref(contents));
    })?;
    Ok(CameraReport { plan, page_width: round(pw), page_height: round(ph), enhance })
}

fn round(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn fmt(v: f64) -> String {
    let s = format!("{:.3}", round(v));
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}
