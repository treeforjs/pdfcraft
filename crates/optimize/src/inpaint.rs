//! Recognize Text ▸ Editable Text and Images: clean the areas whose text became real text out
//! of the page images, so the image stays as the background without the old ink.
//!
//! Each region is a quadrilateral in the page's user space. For every image the page draws
//! (directly or through form XObjects), the regions are mapped into the image's pixels through
//! the inverse of its placement, and the pixels whose centres lie inside are filled with the
//! local paper colour: per channel, the median of the lighter half of a ring of pixels around
//! the region (so neighbouring ink does not tint it). One-bit images get their paper value.
//! Everything outside the regions keeps its samples. 8-bit gray and RGB images are rewritten
//! with Flate (JPEG at quality 90 when they were JPEG), one-bit images as they were encoded
//! (CCITT Group 4 or Flate); other images are left alone and counted as skipped.
//!
//! Work is bounded: at most [`MAX_REGIONS`] regions, [`MAX_PLACEMENTS`] image placements, and
//! for each image about four times its pixel count of region and ring pixels.

use std::collections::BTreeMap;

use pdfcraft_content::Matrix;
use pdfcraft_cos::{Dict, Document, ObjRef, Object, Stream};

use crate::Compression;
use crate::images::{MAX_PIXELS, components, content_bytes, image_stream, is_dct, is_mono, mono_bits, mono_stream, page_resources, pixels};

/// At most this many regions per call.
pub const MAX_REGIONS: usize = 200_000;
/// At most this many image placements are cleaned per page.
pub const MAX_PLACEMENTS: usize = 4_096;

/// What [`crate::inpaint_page_image_regions`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InpaintReport {
    /// Images the page draws.
    pub images: usize,
    /// Images rewritten with regions cleaned.
    pub images_changed: usize,
    /// Images a region falls on that PdfCraft can't decode or re-encode faithfully.
    pub images_skipped: usize,
    /// Pixels filled with paper colour, over all images.
    pub pixels_changed: u64,
    /// Per region: whether it lies wholly within an image that was cleaned. A caller covers the
    /// others some other way (the editable OCR output paints them with paper colour).
    pub covered: Vec<bool>,
}

/// Image placements (CTM at `Do`) in one content stream, through form XObjects.
fn placements(doc: &Document, data: &[u8], resources: &Dict, ctm: Matrix, depth: usize, forms: &mut usize, out: &mut Vec<(ObjRef, Matrix)>) {
    if depth > 12 {
        return;
    }
    let xobjects = resources.get(b"XObject").map(|x| doc.resolve(x)).and_then(|x| x.as_dict().cloned()).unwrap_or_default();
    let mut stack = vec![ctm];
    for op in pdfcraft_content::parse(data).ops {
        if out.len() >= MAX_PLACEMENTS {
            return;
        }
        let top = stack.last().copied().unwrap_or(ctm);
        match op.op.as_slice() {
            b"q" if stack.len() < 4096 => stack.push(top),
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
                    Some(b"Form") => {
                        *forms += 1;
                        if *forms > 256 {
                            continue;
                        }
                        let m = s.dict.get(b"Matrix").map(|m| doc.resolve(m)).and_then(|m| m.as_array().map(|a| Matrix::from_operands(a))).flatten();
                        let res =
                            s.dict.get(b"Resources").map(|r| doc.resolve(r)).and_then(|r| r.as_dict().cloned()).unwrap_or_else(|| resources.clone());
                        let inner = m.unwrap_or(Matrix([1.0, 0.0, 0.0, 1.0, 0.0, 0.0])).then(&top);
                        placements(doc, &s.decoded().unwrap_or_default(), &res, inner, depth + 1, forms, out);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// Decoded samples of an image this module can rewrite.
enum Samples {
    /// 8-bit gray (1) or RGB (3), and whether it was JPEG.
    Colour { px: Vec<u8>, n: usize, w: usize, h: usize, was_dct: bool },
    /// Packed one-bit rows.
    Mono { bits: Vec<u8>, w: usize, h: usize, paper: bool, ccitt: Option<Dict> },
}

fn samples(doc: &Document, s: &Stream) -> Result<Samples, ()> {
    let d = &s.dict;
    if is_mono(doc, d) {
        let (bits, w, h, ink_is_one, ccitt) = mono_bits(s, MAX_PIXELS)?;
        return Ok(Samples::Mono { bits, w, h, paper: !ink_is_one, ccitt });
    }
    if d.contains(b"Decode") || d.contains(b"Mask") || d.int(b"BitsPerComponent") != Some(8) {
        return Err(());
    }
    let n = d.get(b"ColorSpace").and_then(|cs| components(doc, cs)).ok_or(())?;
    let was_dct = is_dct(s).ok_or(())?;
    let (w, h) = (usize::try_from(d.int(b"Width").ok_or(())?).map_err(|_| ())?, usize::try_from(d.int(b"Height").ok_or(())?).map_err(|_| ())?);
    if w == 0 || h == 0 || (w as u64).saturating_mul(h as u64) > MAX_PIXELS {
        return Err(());
    }
    let need = w.checked_mul(h).and_then(|p| p.checked_mul(n)).ok_or(())?;
    let px = if was_dct {
        pixels(s, n).ok_or(())?
    } else {
        s.decoded_complete(need.saturating_add(1 << 16)).map_err(|_| ())?.get(..need).ok_or(())?.to_vec()
    };
    if px.len() != need {
        return Err(());
    }
    Ok(Samples::Colour { px, n, w, h, was_dct })
}

impl Samples {
    fn size(&self) -> (usize, usize) {
        match self {
            Samples::Colour { w, h, .. } | Samples::Mono { w, h, .. } => (*w, *h),
        }
    }

    fn encode(self, d: &Dict) -> Result<Stream, ()> {
        match self {
            Samples::Colour { px, n, w, h, was_dct } => {
                let compression = if was_dct { Compression::Jpeg(90) } else { Compression::Flate };
                let (w, h) = (u32::try_from(w).map_err(|_| ())?, u32::try_from(h).map_err(|_| ())?);
                image_stream(d, &px, n, w, h, compression, was_dct).ok_or(())
            }
            Samples::Mono { bits, w, h, ccitt, .. } => mono_stream(d, &bits, w, h, ccitt),
        }
    }
}

/// Whether pixel centre `p` is inside the convex quad `q`.
fn inside(q: &[[f64; 2]; 4], p: [f64; 2]) -> bool {
    let mut sign = 0.0f64;
    for i in 0..4 {
        let (a, b) = (q[i], q[(i + 1) % 4]);
        let cross = (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]);
        if cross.abs() < 1e-12 {
            continue;
        }
        if sign == 0.0 {
            sign = cross.signum();
        } else if cross.signum() != sign {
            return false;
        }
    }
    true
}

/// Fill the pixels of `samples` inside `quad` (pixel coordinates) with the paper around it.
/// Returns the pixels changed and the work done, or `None` past `budget`.
fn fill(samples: &mut Samples, quad: &[[f64; 2]; 4], budget: u64) -> Option<(u64, u64)> {
    let (w, h) = samples.size();
    let xs = quad.iter().map(|p| p[0]);
    let ys = quad.iter().map(|p| p[1]);
    let (min_x, max_x) = (xs.clone().fold(f64::INFINITY, f64::min), xs.fold(f64::NEG_INFINITY, f64::max));
    let (min_y, max_y) = (ys.clone().fold(f64::INFINITY, f64::min), ys.fold(f64::NEG_INFINITY, f64::max));
    let clamp = |v: f64, hi: usize| v.clamp(0.0, hi as f64) as usize;
    let (x0, x1) = (clamp(min_x.floor(), w), clamp(max_x.ceil(), w));
    let (y0, y1) = (clamp(min_y.floor(), h), clamp(max_y.ceil(), h));
    if x1 <= x0 || y1 <= y0 {
        return Some((0, 0));
    }
    let band = ((0.25 * (max_y - min_y).min(max_x - min_x)).round() as usize).clamp(2, 64);
    let (rx0, rx1) = (x0.saturating_sub(band), x1.saturating_add(band).min(w));
    let (ry0, ry1) = (y0.saturating_sub(band), y1.saturating_add(band).min(h));
    let work = ((rx1 - rx0) as u64).saturating_mul((ry1 - ry0) as u64);
    if work > budget {
        return None;
    }
    let mut changed = 0u64;
    match samples {
        Samples::Mono { bits, paper, .. } => {
            let stride = w.div_ceil(8);
            for y in y0..y1 {
                for x in x0..x1 {
                    if !inside(quad, [x as f64 + 0.5, y as f64 + 0.5]) {
                        continue;
                    }
                    let Some(byte) = y.checked_mul(stride).and_then(|o| o.checked_add(x / 8)).and_then(|i| bits.get_mut(i)) else { continue };
                    let mask = 0x80u8 >> (x % 8);
                    let was = *byte & mask != 0;
                    if was != *paper {
                        *byte ^= mask;
                        changed += 1;
                    }
                }
            }
        }
        Samples::Colour { px, n, .. } => {
            let n = *n;
            let at = |x: usize, y: usize| y.checked_mul(w).and_then(|o| o.checked_add(x)).and_then(|o| o.checked_mul(n));
            // The ring around the region, sampled (at most about 4096 pixels).
            let ring_area = work.saturating_sub(((x1 - x0) as u64).saturating_mul((y1 - y0) as u64)).max(1);
            let step = (ring_area / 4096).max(1) as usize;
            let mut ring: Vec<(u32, [u8; 3])> = Vec::new();
            let mut k = 0usize;
            for y in ry0..ry1 {
                for x in rx0..rx1 {
                    if (x0..x1).contains(&x) && (y0..y1).contains(&y) {
                        continue;
                    }
                    k += 1;
                    if !k.is_multiple_of(step) {
                        continue;
                    }
                    let Some(p) = at(x, y).and_then(|o| px.get(o..o + n)) else { continue };
                    let c = if n == 1 { [p[0]; 3] } else { [p[0], p.get(1).copied().unwrap_or(p[0]), p.get(2).copied().unwrap_or(p[0])] };
                    let luma = 77 * u32::from(c[0]) + 150 * u32::from(c[1]) + 29 * u32::from(c[2]);
                    ring.push((luma, c));
                }
            }
            if ring.is_empty() {
                return Some((0, work));
            }
            ring.sort_unstable_by_key(|(l, _)| *l);
            let light = ring.get(ring.len() / 2..).unwrap_or(&[]);
            let paper: [u8; 3] = [0, 1, 2].map(|ch| {
                let mut v: Vec<u8> = light.iter().map(|(_, c)| c[ch]).collect();
                v.sort_unstable();
                v.get(v.len() / 2).copied().unwrap_or(255)
            });
            for y in y0..y1 {
                for x in x0..x1 {
                    if !inside(quad, [x as f64 + 0.5, y as f64 + 0.5]) {
                        continue;
                    }
                    let Some(p) = at(x, y).and_then(|o| px.get_mut(o..o + n)) else { continue };
                    let want = &paper[..n.min(3)];
                    if &*p != want {
                        p.copy_from_slice(want);
                        changed += 1;
                    }
                }
            }
        }
    }
    Some((changed, work))
}

pub(crate) fn inpaint(doc: &mut Document, page: ObjRef, regions: &[[[f64; 2]; 4]]) -> InpaintReport {
    let mut report = InpaintReport { covered: vec![false; regions.len()], ..Default::default() };
    let Some(d) = doc.get(page).as_dict().cloned() else { return report };
    let Some(contents) = d.get(b"Contents") else { return report };
    let res = page_resources(doc, page).unwrap_or_default();
    let mut found = Vec::new();
    let mut forms = 0;
    placements(doc, &content_bytes(doc, contents), &res, Matrix([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]), 0, &mut forms, &mut found);
    let mut by_image: BTreeMap<ObjRef, Vec<Matrix>> = BTreeMap::new();
    for (r, m) in found {
        by_image.entry(r).or_default().push(m);
    }
    for (r, matrices) in by_image {
        report.images += 1;
        let obj = doc.get(r);
        let Object::Stream(s) = &*obj else { continue };
        // Which regions fall on this image, in its pixels.
        let (iw, ih) = (s.dict.int(b"Width").unwrap_or(0) as f64, s.dict.int(b"Height").unwrap_or(0) as f64);
        if !(iw > 0.0 && ih > 0.0) {
            continue;
        }
        let mut hits: Vec<(usize, [[f64; 2]; 4], bool)> = Vec::new();
        for m in &matrices {
            let Some(inv) = m.invert() else { continue };
            for (i, q) in regions.iter().enumerate() {
                let px = q.map(|[x, y]| {
                    let (u, v) = inv.apply(x, y);
                    [u * iw, (1.0 - v) * ih]
                });
                if px.iter().flatten().any(|v| !v.is_finite()) {
                    continue;
                }
                let within = px.iter().all(|p| (-0.5..=iw + 0.5).contains(&p[0]) && (-0.5..=ih + 0.5).contains(&p[1]));
                let touches =
                    px.iter().any(|p| p[0] > 0.0) && px.iter().any(|p| p[0] < iw) && px.iter().any(|p| p[1] > 0.0) && px.iter().any(|p| p[1] < ih);
                if touches {
                    hits.push((i, px, within));
                }
            }
        }
        if hits.is_empty() {
            continue;
        }
        let Ok(mut decoded) = samples(doc, s) else {
            report.images_skipped += 1;
            continue;
        };
        let (w, h) = decoded.size();
        let mut budget = (w as u64).saturating_mul(h as u64).saturating_mul(4).saturating_add(1 << 20);
        let mut changed = 0u64;
        let mut covered = Vec::new();
        for (i, quad, within) in hits {
            match fill(&mut decoded, &quad, budget) {
                Some((c, work)) => {
                    changed = changed.saturating_add(c);
                    budget = budget.saturating_sub(work);
                    if within {
                        covered.push(i);
                    }
                }
                None => budget = 0,
            }
        }
        if changed > 0 {
            match decoded.encode(&s.dict) {
                Ok(new) => {
                    doc.set(r, Object::Stream(new));
                    report.images_changed += 1;
                    report.pixels_changed = report.pixels_changed.saturating_add(changed);
                }
                Err(()) => {
                    report.images_skipped += 1;
                    continue;
                }
            }
        }
        for i in covered {
            if let Some(c) = report.covered.get_mut(i) {
                *c = true;
            }
        }
    }
    report
}
