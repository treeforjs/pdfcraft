//! Image resampling and recompression.

use std::collections::{HashMap, HashSet};

use pdfcraft_content::Matrix;
use pdfcraft_cos::{Dict, Document, ObjRef, Object, Stream};

use crate::{Compression, ImageSettings, MonoCompression, MonoSettings, OptimizeError, Report, Settings, Stage};

/// Images with more pixels than this are left alone (memory).
pub(crate) const MAX_PIXELS: u64 = 80_000_000;

/// The resources of a page, inherited from the page tree if the page has none.
pub(crate) fn page_resources(doc: &Document, page: ObjRef) -> Option<Dict> {
    let mut node = Some(page);
    for _ in 0..64 {
        let d = doc.get(node?).as_dict()?.clone();
        if let Some(r) = d.get(b"Resources") {
            return doc.resolve(r).as_dict().cloned();
        }
        node = d.reference(b"Parent");
    }
    None
}

pub(crate) fn content_bytes(doc: &Document, contents: &Object) -> Vec<u8> {
    match &*doc.resolve(contents) {
        Object::Stream(s) => s.decoded().unwrap_or_default(),
        Object::Array(a) => a
            .iter()
            .flat_map(|c| match &*doc.resolve(c) {
                Object::Stream(s) => {
                    let mut v = s.decoded().unwrap_or_default();
                    v.push(b'\n');
                    v
                }
                _ => Vec::new(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Walk one content stream, recording each image's displayed resolution (pixels per inch, the
/// smaller of the two axes) and descending into form XObjects.
fn walk(doc: &Document, data: &[u8], resources: &Dict, ctm: Matrix, depth: usize, seen: &mut HashSet<ObjRef>, out: &mut HashMap<ObjRef, f64>) {
    if depth > 12 {
        return;
    }
    let xobjects = resources.get(b"XObject").map(|x| doc.resolve(x)).and_then(|x| x.as_dict().cloned()).unwrap_or_default();
    let mut stack = vec![ctm];
    for op in pdfcraft_content::parse(data).ops {
        let top = stack.last().copied().unwrap_or(ctm);
        match op.op.as_slice() {
            b"q" => stack.push(top),
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
                    Some(b"Image") => {
                        let (w, h) = (s.dict.int(b"Width").unwrap_or(0) as f64, s.dict.int(b"Height").unwrap_or(0) as f64);
                        let [a, b, c, d, ..] = top.0;
                        let (sx, sy) = ((a * a + b * b).sqrt(), (c * c + d * d).sqrt());
                        if w > 0.0 && h > 0.0 && sx > 1e-6 && sy > 1e-6 {
                            let ppi = (w / (sx / 72.0)).min(h / (sy / 72.0));
                            let e = out.entry(r).or_insert(f64::INFINITY);
                            *e = e.min(ppi);
                        }
                    }
                    Some(b"Form") => {
                        if !seen.insert(r) && depth > 4 {
                            continue;
                        }
                        let m = s.dict.get(b"Matrix").map(|m| doc.resolve(m)).and_then(|m| m.as_array().map(|a| Matrix::from_operands(a))).flatten();
                        let res =
                            s.dict.get(b"Resources").map(|r| doc.resolve(r)).and_then(|r| r.as_dict().cloned()).unwrap_or_else(|| resources.clone());
                        let inner = m.unwrap_or(Matrix([1.0, 0.0, 0.0, 1.0, 0.0, 0.0])).then(&top);
                        walk(doc, &s.decoded().unwrap_or_default(), &res, inner, depth + 1, seen, out);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// The effective resolution (ppi) of every image drawn by `pages`: the smallest over its uses.
pub fn effective_resolutions(doc: &Document, pages: &[ObjRef]) -> HashMap<ObjRef, f64> {
    let mut out = HashMap::new();
    let mut seen = HashSet::new();
    for p in pages {
        let Some(d) = doc.get(*p).as_dict().cloned() else { continue };
        let Some(contents) = d.get(b"Contents") else { continue };
        let res = page_resources(doc, *p).unwrap_or_default();
        walk(doc, &content_bytes(doc, contents), &res, Matrix([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]), 0, &mut seen, &mut out);
    }
    out
}

/// Colour components of a colour space PdfCraft resamples: gray (1) or RGB (3).
pub(crate) fn components(doc: &Document, cs: &Object) -> Option<usize> {
    let cs = doc.resolve(cs);
    match &*cs {
        Object::Name(n) => match n.as_slice() {
            b"DeviceGray" | b"G" => Some(1),
            b"DeviceRGB" | b"RGB" => Some(3),
            _ => None,
        },
        Object::Array(a) => match a.first()?.as_name()? {
            b"ICCBased" => match &*doc.resolve(a.get(1)?) {
                Object::Stream(icc) => match icc.dict.int(b"N")? {
                    1 => Some(1),
                    3 => Some(3),
                    _ => None,
                },
                _ => None,
            },
            b"CalGray" => Some(1),
            b"CalRGB" => Some(3),
            _ => None,
        },
        _ => None,
    }
}

pub(crate) fn is_dct(s: &Stream) -> Option<bool> {
    match s.dict.get(b"Filter") {
        None => Some(false),
        Some(Object::Name(n)) => match n.as_slice() {
            b"DCTDecode" | b"DCT" => Some(true),
            b"FlateDecode" | b"Fl" | b"LZWDecode" | b"LZW" | b"RunLengthDecode" | b"RL" | b"ASCII85Decode" | b"A85" | b"ASCIIHexDecode" | b"AHx" => {
                Some(false)
            }
            _ => None,
        },
        Some(Object::Array(a)) => {
            let names: Vec<&[u8]> = a.iter().filter_map(Object::as_name).collect();
            if names.len() != a.len() {
                return None;
            }
            match names.as_slice() {
                [b"DCTDecode" | b"DCT"] => Some(true),
                n if n.iter().all(|f| {
                    matches!(
                        *f,
                        b"FlateDecode"
                            | b"Fl"
                            | b"LZWDecode"
                            | b"LZW"
                            | b"RunLengthDecode"
                            | b"RL"
                            | b"ASCII85Decode"
                            | b"A85"
                            | b"ASCIIHexDecode"
                            | b"AHx"
                    )
                }) =>
                {
                    Some(false)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// 8-bit samples of an image (`n` components), decoded.
pub(crate) fn pixels(s: &Stream, n: usize) -> Option<Vec<u8>> {
    let (w, h) = (s.dict.int(b"Width")? as usize, s.dict.int(b"Height")? as usize);
    if is_dct(s)? {
        let img = image::load_from_memory_with_format(&s.raw, image::ImageFormat::Jpeg).ok()?;
        // Adobe CMYK/YCCK JPEGs aren't handled (the colour space check excludes them).
        let v = if n == 1 { img.into_luma8().into_raw() } else { img.into_rgb8().into_raw() };
        (v.len() == w * h * n).then_some(v)
    } else {
        let v = s.decoded().ok()?;
        (v.len() >= w * h * n).then(|| v[..w * h * n].to_vec())
    }
}

fn resample(px: &[u8], n: usize, w: u32, h: u32, nw: u32, nh: u32) -> Option<Vec<u8>> {
    use image::imageops::{FilterType, resize};
    Some(match n {
        1 => resize(&image::GrayImage::from_raw(w, h, px.to_vec())?, nw, nh, FilterType::CatmullRom).into_raw(),
        _ => resize(&image::RgbImage::from_raw(w, h, px.to_vec())?, nw, nh, FilterType::CatmullRom).into_raw(),
    })
}

fn jpeg(px: &[u8], n: usize, w: u32, h: u32, quality: u8) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100));
    let ty = if n == 1 { image::ExtendedColorType::L8 } else { image::ExtendedColorType::Rgb8 };
    enc.encode(px, w, h, ty).ok()?;
    Some(out)
}

/// A new image stream from 8-bit samples: the original dictionary with the new size and filter.
pub(crate) fn image_stream(old: &Dict, px: &[u8], n: usize, w: u32, h: u32, compression: Compression, was_dct: bool) -> Option<Stream> {
    let mut d = old.clone();
    for k in [&b"Filter"[..], b"DecodeParms", b"Length", b"DL"] {
        d.remove(k);
    }
    d.set(b"Width".to_vec(), Object::Int(w as i64));
    d.set(b"Height".to_vec(), Object::Int(h as i64));
    d.set(b"BitsPerComponent".to_vec(), Object::Int(8));
    let use_jpeg = match compression {
        Compression::Jpeg(_) => true,
        Compression::Flate => false,
        Compression::Retain => was_dct,
    };
    if use_jpeg {
        let q = match compression {
            Compression::Jpeg(q) => q,
            _ => 80,
        };
        let data = jpeg(px, n, w, h, q)?;
        d.set(b"Filter".to_vec(), Object::name("DCTDecode"));
        Some(Stream::from_raw(d, data))
    } else {
        Some(Stream::flate(d, px))
    }
}

/// One image: resample and/or recompress per `settings`; `ppi` is its effective resolution.
/// Returns the new image and its new soft mask, if the result is smaller.
fn process(doc: &Document, s: &Stream, ppi: f64, settings: &ImageSettings, n: usize) -> Option<(Stream, Option<(ObjRef, Stream)>)> {
    let d = &s.dict;
    if d.contains(b"Decode") || d.contains(b"Mask") || d.name(b"ImageMask").is_some() || matches!(d.get(b"ImageMask"), Some(Object::Bool(true))) {
        return None;
    }
    if d.int(b"BitsPerComponent") != Some(8) {
        return None;
    }
    let was_dct = is_dct(s)?;
    let (w, h) = (d.int(b"Width")? as u32, d.int(b"Height")? as u32);
    if w == 0 || h == 0 || (w as u64) * (h as u64) > MAX_PIXELS {
        return None;
    }
    let resize = settings.downsample && ppi.is_finite() && ppi > settings.above_ppi && settings.target_ppi > 0.0;
    let recompress = matches!(settings.compression, Compression::Jpeg(_)) || (matches!(settings.compression, Compression::Flate) && was_dct);
    if !resize && !recompress {
        return None;
    }
    // Tiny images (icons, rules) don't benefit and JPEG would blur them.
    if !resize && (w as u64) * (h as u64) < 64 * 64 {
        return None;
    }
    let (nw, nh) = if resize {
        let k = settings.target_ppi / ppi;
        (((w as f64 * k).round() as u32).max(1), ((h as f64 * k).round() as u32).max(1))
    } else {
        (w, h)
    };
    let px = pixels(s, n)?;
    let px = if (nw, nh) != (w, h) { resample(&px, n, w, h, nw, nh)? } else { px };
    let new = image_stream(d, &px, n, nw, nh, settings.compression, was_dct)?;
    // A soft mask must keep the image's dimensions: resample it with the image (lossless).
    let mut smask = None;
    if let Some(mr) = d.reference(b"SMask") {
        let mobj = doc.get(mr);
        let Object::Stream(m) = &*mobj else { return None };
        if (nw, nh) != (w, h) {
            if m.dict.int(b"BitsPerComponent") != Some(8) || m.dict.contains(b"Matte") || m.dict.contains(b"Decode") {
                return None;
            }
            let (mw, mh) = (m.dict.int(b"Width")? as u32, m.dict.int(b"Height")? as u32);
            let mpx = pixels(m, 1)?;
            let mpx = resample(&mpx, 1, mw, mh, nw, nh)?;
            smask = Some((mr, image_stream(&m.dict, &mpx, 1, nw, nh, Compression::Flate, false)?));
        }
    }
    let before = s.raw.len()
        + smask.as_ref().map_or(0, |(r, _)| match &*doc.get(*r) {
            Object::Stream(m) => m.raw.len(),
            _ => 0,
        });
    let after = new.raw.len() + smask.as_ref().map_or(0, |(_, m)| m.raw.len());
    (after < before).then_some((new, smask))
}

/// A one-bit image: an image mask, or one-bit samples in a one-component colour space.
pub(crate) fn is_mono(doc: &Document, d: &Dict) -> bool {
    if matches!(d.get(b"ImageMask"), Some(Object::Bool(true))) {
        return d.get(b"BitsPerComponent").is_none_or(|b| matches!(b, Object::Int(1)));
    }
    d.int(b"BitsPerComponent") == Some(1) && d.get(b"ColorSpace").and_then(|cs| components(doc, cs)) == Some(1)
}

/// Area-average downsampling of packed one-bit rows to `nw` × `nh` (both no larger than the
/// source). Each output pixel covers a block of source pixels and takes the majority value;
/// ties go to `ink` so one-pixel strokes survive a 2:1 reduction.
fn downsample_bits(bits: &[u8], w: usize, h: usize, nw: usize, nh: usize, ink: bool) -> Option<Vec<u8>> {
    if nw == 0 || nh == 0 || nw > w || nh > h || w > 1 << 20 {
        return None;
    }
    let (stride, nstride) = (w.div_ceil(8), nw.div_ceil(8));
    // Output column of each source column (a partition: every output column gets at least one).
    let col: Vec<usize> = (0..w).map(|x| ((x as u64 * nw as u64) / w as u64) as usize).collect();
    let mut widths = vec![0u32; nw];
    for &c in &col {
        *widths.get_mut(c)? += 1;
    }
    let mut out = vec![0u8; nstride.checked_mul(nh)?];
    let mut ink_count = vec![0u32; nw];
    let mut rows_in_block = 0u32;
    let mut oy = 0usize;
    for y in 0..h {
        let row = bits.get(y * stride..(y + 1) * stride)?;
        for (x, &c) in col.iter().enumerate() {
            let bit = (row.get(x / 8)? >> (7 - x % 8)) & 1 == 1;
            if bit == ink {
                *ink_count.get_mut(c)? += 1;
            }
        }
        rows_in_block += 1;
        let next = (((y + 1) as u64 * nh as u64) / h as u64) as usize;
        if next != oy || y + 1 == h {
            let orow = out.get_mut(oy * nstride..(oy + 1) * nstride)?;
            for (ox, (&n, &cw)) in ink_count.iter().zip(&widths).enumerate() {
                let area = u64::from(cw) * u64::from(rows_in_block);
                let inked = 2 * u64::from(n) >= area;
                if inked == ink {
                    *orow.get_mut(ox / 8)? |= 0x80 >> (ox % 8);
                }
            }
            ink_count.fill(0);
            rows_in_block = 0;
            oy = next;
        }
    }
    Some(out)
}

/// A one-bit image: optionally downsampled, then stored as CCITT Group 4 or Flate. Only
/// images whose samples decode completely through general filters are touched (images that
/// are already CCITT or JBIG2, indirect filter descriptions and truncated LZW data are left
/// alone), and the new stream is returned only if it is smaller. Without downsampling the
/// samples are unchanged.
fn process_mono(s: &Stream, ppi: f64, st: &MonoSettings, jbig2_ok: bool) -> Option<Stream> {
    let d = &s.dict;
    let (w, h) = (usize::try_from(d.int(b"Width")?).ok()?, usize::try_from(d.int(b"Height")?).ok()?);
    // Group 4 takes at most 2^20 columns; the bound also keeps the per-column tables small.
    if w == 0 || h == 0 || w > 1 << 20 || (w as u64).saturating_mul(h as u64) > MAX_PIXELS {
        return None;
    }
    let resize = st.downsample && ppi.is_finite() && ppi > st.above_ppi && st.target_ppi > 0.0 && !d.contains(b"SMask") && !d.contains(b"Mask");
    if !resize && st.compression == MonoCompression::Retain {
        return None;
    }
    let stride = w.div_ceil(8);
    let need = stride.checked_mul(h)?;
    let data = s.decoded_complete(need.saturating_add(1 << 16)).ok()?;
    let bits = data.get(..need)?;
    let (nw, nh) = if resize {
        let k = st.target_ppi / ppi;
        (((w as f64 * k).round() as usize).clamp(1, w), ((h as f64 * k).round() as usize).clamp(1, h))
    } else {
        (w, h)
    };
    // Ink is sample value 0 unless a /Decode array inverts the samples.
    let ink = match d.get(b"Decode").and_then(Object::as_array).and_then(|a| a.first()) {
        Some(Object::Int(v)) => *v >= 1,
        Some(Object::Real(v)) => *v > 0.5,
        _ => false,
    };
    let resized;
    let bits = if (nw, nh) != (w, h) {
        resized = downsample_bits(bits, w, h, nw, nh, ink)?;
        &resized[..]
    } else {
        bits
    };
    let mut nd = d.clone();
    for k in [&b"Filter"[..], b"DecodeParms", b"Length", b"DL"] {
        nd.remove(k);
    }
    nd.set(b"Width".to_vec(), Object::Int(nw as i64));
    nd.set(b"Height".to_vec(), Object::Int(nh as i64));
    let compression = if st.compression == MonoCompression::Jbig2 && !jbig2_ok { MonoCompression::CcittG4 } else { st.compression };
    let new = match compression {
        MonoCompression::Jbig2 => {
            // JBIG2 codes the samples losslessly; only a smaller stream is useful.
            let data = pdfcraft_filters::encode_jbig2_generic(bits, nw, nh, s.raw.len()).ok()?;
            nd.set(b"Filter".to_vec(), Object::name("JBIG2Decode"));
            Stream::from_raw(nd, data)
        }
        MonoCompression::CcittG4 => {
            // Only a smaller stream is useful, so encoding stops past the current size. Either
            // polarity round-trips the samples; keep the smaller.
            let limit = s.raw.len();
            let plain = pdfcraft_filters::encode_ccitt_g4(bits, nw, nh, false, limit).ok();
            let inverted = pdfcraft_filters::encode_ccitt_g4(bits, nw, nh, true, plain.as_ref().map_or(limit, |p| p.len().saturating_sub(1))).ok();
            let (data, black_is_1) = match (plain, inverted) {
                (_, Some(i)) => (i, true),
                (Some(p), None) => (p, false),
                (None, None) => return None,
            };
            let mut parms = Dict::new();
            parms.set(b"K".to_vec(), Object::Int(-1));
            parms.set(b"Columns".to_vec(), Object::Int(nw as i64));
            parms.set(b"Rows".to_vec(), Object::Int(nh as i64));
            if black_is_1 {
                parms.set(b"BlackIs1".to_vec(), Object::Bool(true));
            }
            nd.set(b"Filter".to_vec(), Object::name("CCITTFaxDecode"));
            nd.set(b"DecodeParms".to_vec(), Object::Dict(parms));
            Stream::from_raw(nd, data)
        }
        MonoCompression::Flate | MonoCompression::Retain => Stream::flate(nd, bits),
    };
    (new.raw.len() < s.raw.len()).then_some(new)
}

pub(crate) fn run(
    doc: &mut Document,
    pages: &[ObjRef],
    settings: &Settings,
    report: &mut Report,
    progress: &mut dyn FnMut(Stage) -> bool,
) -> Result<(), OptimizeError> {
    let ppi = effective_resolutions(doc, pages);
    // Soft masks are processed with their image, not on their own.
    let masks: HashSet<ObjRef> = ppi
        .keys()
        .filter_map(|r| match &*doc.get(*r) {
            Object::Stream(s) => s.dict.reference(b"SMask"),
            _ => None,
        })
        .collect();
    let mut refs: Vec<(ObjRef, f64)> = ppi.into_iter().filter(|(r, _)| !masks.contains(r)).collect();
    refs.sort_by_key(|(r, _)| r.num);
    let total = refs.len();
    for (done, (r, ppi)) in refs.into_iter().enumerate() {
        if !progress(Stage::Images { done, total }) {
            return Err(OptimizeError::Cancelled);
        }
        let obj = doc.get(r);
        let Object::Stream(s) = &*obj else { continue };
        report.images += 1;
        if is_mono(doc, &s.dict) {
            let jbig2_ok = settings.compatibility.is_none_or(|v| v >= (1, 4));
            let Some(new) = process_mono(s, ppi, &settings.mono, jbig2_ok) else { continue };
            let resized = new.dict.int(b"Width") != s.dict.int(b"Width") || new.dict.int(b"Height") != s.dict.int(b"Height");
            report.image_bytes_before += s.raw.len();
            report.image_bytes_after += new.raw.len();
            if resized {
                report.images_resampled += 1;
            } else {
                report.images_recompressed += 1;
            }
            match new.dict.name(b"Filter") {
                Some(b"CCITTFaxDecode") => report.images_ccitt += 1,
                Some(b"JBIG2Decode") => report.images_jbig2 += 1,
                _ => {}
            }
            doc.set(r, Object::Stream(new));
            continue;
        }
        let Some(n) = s.dict.get(b"ColorSpace").and_then(|cs| components(doc, cs)) else { continue };
        let st = if n == 1 { &settings.gray } else { &settings.color };
        let Some((new, smask)) = process(doc, s, ppi, st, n) else { continue };
        let resized = new.dict.int(b"Width") != s.dict.int(b"Width") || new.dict.int(b"Height") != s.dict.int(b"Height");
        report.image_bytes_before += s.raw.len();
        report.image_bytes_after += new.raw.len();
        if resized {
            report.images_resampled += 1;
        } else {
            report.images_recompressed += 1;
        }
        doc.set(r, Object::Stream(new));
        if let Some((mr, m)) = smask {
            doc.set(mr, Object::Stream(m));
        }
    }
    if !progress(Stage::Images { done: total, total }) {
        return Err(OptimizeError::Cancelled);
    }
    Ok(())
}

/// What [`crate::despeckle_page_images`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DespeckleReport {
    /// Images the pages draw (soft masks excluded).
    pub images: usize,
    /// Images rewritten because despeckling changed them.
    pub images_changed: usize,
    /// Images left alone because PdfCraft can't decode or re-encode them faithfully (JBIG2,
    /// JPEG 2000, CMYK and other colour spaces, decode arrays, more than 8 bits, an effective
    /// resolution outside 36–4800 ppi) or they were too large or fragmented.
    pub images_skipped: usize,
    /// The specks and pixels changed over all images.
    pub stats: crate::despeckle::Stats,
}

/// The single `/CCITTFaxDecode` filter's parameters, when that is the image's only filter.
fn ccitt_params(s: &Stream) -> Option<Option<Dict>> {
    let single = match s.dict.get(b"Filter")? {
        Object::Name(n) => n.as_slice(),
        Object::Array(a) if a.len() == 1 => a.first()?.as_name()?,
        _ => return Some(None),
    };
    if !matches!(single, b"CCITTFaxDecode" | b"CCF") {
        return Some(None);
    }
    let parms = match s.dict.get(b"DecodeParms") {
        None | Some(Object::Null) => Dict::new(),
        Some(Object::Dict(d)) => d.clone(),
        Some(Object::Array(a)) if a.len() == 1 => match a.first()? {
            Object::Dict(d) => d.clone(),
            Object::Null => Dict::new(),
            _ => return None,
        },
        _ => return None,
    };
    Some(Some(parms))
}

/// Packed one-bit rows decoded from CCITT fax data, as a PDF reader sees the samples.
fn ccitt_samples(data: &[u8], parms: &Dict, w: usize, h: usize) -> Option<Vec<u8>> {
    let flag = |k: &[u8], default: bool| match parms.get(k) {
        Some(Object::Bool(b)) => Some(*b),
        None => Some(default),
        _ => None,
    };
    let k = parms.int(b"K").unwrap_or(0);
    let columns = parms.int(b"Columns").unwrap_or(1728);
    let rows = parms.int(b"Rows").unwrap_or(0);
    if usize::try_from(columns).ok()? != w || (rows != 0 && usize::try_from(rows).ok()? != h) {
        return None;
    }
    let black_is_1 = flag(b"BlackIs1", false)?;
    let encoding = match k {
        k if k < 0 => hayro_ccitt::EncodingMode::Group4,
        0 => hayro_ccitt::EncodingMode::Group3_1D,
        k => hayro_ccitt::EncodingMode::Group3_2D { k: u32::try_from(k).ok()? },
    };
    let settings = hayro_ccitt::DecodeSettings {
        columns: u32::try_from(w).ok()?,
        rows: u32::try_from(h).ok()?,
        end_of_block: flag(b"EndOfBlock", true)?,
        end_of_line: flag(b"EndOfLine", false)?,
        rows_are_byte_aligned: flag(b"EncodedByteAlign", false)?,
        encoding,
        invert_black: false,
    };
    /// Writes pixels into packed rows; anything past the image is ignored.
    struct Sink {
        out: Vec<u8>,
        stride: usize,
        w: usize,
        h: usize,
        x: usize,
        y: usize,
        black_is_1: bool,
    }
    impl Sink {
        fn pixel(&mut self, white: bool) {
            if self.x < self.w && self.y < self.h {
                // A PDF sample is 1 for white unless /BlackIs1, when black is 1.
                if white != self.black_is_1
                    && let Some(b) = self.out.get_mut(self.y * self.stride + self.x / 8)
                {
                    *b |= 0x80 >> (self.x % 8);
                }
            }
            self.x = self.x.saturating_add(1);
        }
    }
    impl hayro_ccitt::Decoder for Sink {
        fn push_pixel(&mut self, white: bool) {
            self.pixel(white);
        }
        fn push_pixel_chunk(&mut self, white: bool, chunks: u32) {
            for _ in 0..(chunks as usize).saturating_mul(8).min(self.w.saturating_sub(self.x).saturating_add(8)) {
                self.pixel(white);
            }
        }
        fn next_line(&mut self) {
            self.x = 0;
            self.y = self.y.saturating_add(1);
        }
    }
    let stride = w.div_ceil(8);
    let mut sink = Sink { out: vec![0; stride.checked_mul(h)?], stride, w, h, x: 0, y: 0, black_is_1 };
    hayro_ccitt::decode(data, &mut sink, &mut hayro_ccitt::DecoderContext::new(settings)).ok()?;
    // Only a complete image is rewritten; a truncated one would lose rows.
    (sink.y >= h).then_some(sink.out)
}

/// The packed one-bit samples of a mono image: `(bits, width, height, ink_is_one, ccitt)`,
/// where `ccitt` holds the CCITT parameters when the image was fax-encoded.
pub(crate) type MonoBits = (Vec<u8>, usize, usize, bool, Option<Dict>);

pub(crate) fn mono_bits(s: &Stream, max_pixels: u64) -> Result<MonoBits, ()> {
    let d = &s.dict;
    let (w, h) = (usize::try_from(d.int(b"Width").ok_or(())?).map_err(|_| ())?, usize::try_from(d.int(b"Height").ok_or(())?).map_err(|_| ())?);
    if w == 0 || h == 0 || w > 1 << 20 || (w as u64).saturating_mul(h as u64) > max_pixels {
        return Err(());
    }
    let stride = w.div_ceil(8);
    let need = stride.checked_mul(h).ok_or(())?;
    let ccitt = ccitt_params(s).ok_or(())?;
    let bits = match &ccitt {
        Some(parms) => {
            if d.contains(b"F") || d.contains(b"FFilter") {
                return Err(());
            }
            ccitt_samples(&s.raw, parms, w, h).ok_or(())?
        }
        None => s.decoded_complete(need.saturating_add(1 << 16)).map_err(|_| ())?.get(..need).ok_or(())?.to_vec(),
    };
    let ink_is_one = match d.get(b"Decode").and_then(Object::as_array).and_then(|a| a.first()) {
        Some(Object::Int(v)) => *v >= 1,
        Some(Object::Real(v)) => *v > 0.5,
        _ => false,
    };
    Ok((bits, w, h, ink_is_one, ccitt))
}

/// A mono image stream with new samples, re-encoded as it was (CCITT Group 4 or Flate).
pub(crate) fn mono_stream(d: &Dict, bits: &[u8], w: usize, h: usize, ccitt: Option<Dict>) -> Result<Stream, ()> {
    let need = w.div_ceil(8).checked_mul(h).ok_or(())?;
    let mut nd = d.clone();
    for k in [&b"Filter"[..], b"DecodeParms", b"Length", b"DL"] {
        nd.remove(k);
    }
    Ok(match ccitt {
        Some(parms) => {
            let black_is_1 = matches!(parms.get(b"BlackIs1"), Some(Object::Bool(true)));
            let data = pdfcraft_filters::encode_ccitt_g4(bits, w, h, black_is_1, need.saturating_mul(4).saturating_add(1 << 16)).map_err(|_| ())?;
            let mut p = Dict::new();
            p.set(b"K".to_vec(), Object::Int(-1));
            p.set(b"Columns".to_vec(), Object::Int(w as i64));
            p.set(b"Rows".to_vec(), Object::Int(h as i64));
            if black_is_1 {
                p.set(b"BlackIs1".to_vec(), Object::Bool(true));
            }
            nd.set(b"Filter".to_vec(), Object::name("CCITTFaxDecode"));
            nd.set(b"DecodeParms".to_vec(), Object::Dict(p));
            Stream::from_raw(nd, data)
        }
        None => Stream::flate(nd, bits),
    })
}

/// One one-bit image, despeckled: the new stream, or `Ok(None)` when nothing changed.
fn despeckle_mono(s: &Stream, ppi: f64, options: crate::despeckle::Options) -> Result<Option<(Stream, crate::despeckle::Stats)>, ()> {
    let (mut bits, w, h, ink_is_one, ccitt) = mono_bits(s, crate::despeckle::MAX_PIXELS)?;
    let (Ok(width), Ok(height)) = (u32::try_from(w), u32::try_from(h)) else { return Err(()) };
    let stats = crate::despeckle::despeckle_bits(&mut bits, width, height, ink_is_one, ppi, options).map_err(|_| ())?;
    if !stats.changed() {
        return Ok(None);
    }
    Ok(Some((mono_stream(&s.dict, &bits, w, h, ccitt)?, stats)))
}

/// One 8-bit gray or RGB image, despeckled (JPEG images are re-encoded at quality 90, others
/// with Flate); `Ok(None)` when nothing changed.
fn despeckle_colour(
    doc: &Document,
    s: &Stream,
    ppi: f64,
    options: crate::despeckle::Options,
) -> Result<Option<(Stream, crate::despeckle::Stats)>, ()> {
    let d = &s.dict;
    if d.contains(b"Decode") || d.contains(b"Mask") || d.int(b"BitsPerComponent") != Some(8) {
        return Err(());
    }
    let n = d.get(b"ColorSpace").and_then(|cs| components(doc, cs)).ok_or(())?;
    let was_dct = is_dct(s).ok_or(())?;
    let (w, h) = (u32::try_from(d.int(b"Width").ok_or(())?).map_err(|_| ())?, u32::try_from(d.int(b"Height").ok_or(())?).map_err(|_| ())?);
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > crate::despeckle::MAX_PIXELS {
        return Err(());
    }
    let need = (w as usize).checked_mul(h as usize).and_then(|p| p.checked_mul(n)).ok_or(())?;
    let mut px = if was_dct {
        pixels(s, n).ok_or(())?
    } else {
        // Lossless rewriting: the samples must decode completely (no truncated data published).
        s.decoded_complete(need.saturating_add(1 << 16)).map_err(|_| ())?.get(..need).ok_or(())?.to_vec()
    };
    if px.len() != need {
        return Err(());
    }
    let stats = crate::despeckle::despeckle_pixels(&mut px, w, h, n, ppi, options).map_err(|_| ())?;
    if !stats.changed() {
        return Ok(None);
    }
    let compression = if was_dct { Compression::Jpeg(90) } else { Compression::Flate };
    Ok(Some((image_stream(d, &px, n, w, h, compression, was_dct).ok_or(())?, stats)))
}

pub(crate) fn despeckle(doc: &mut Document, pages: &[ObjRef], options: crate::despeckle::Options) -> DespeckleReport {
    let mut report = DespeckleReport::default();
    let ppi = effective_resolutions(doc, pages);
    let masks: HashSet<ObjRef> = ppi
        .keys()
        .filter_map(|r| match &*doc.get(*r) {
            Object::Stream(s) => s.dict.reference(b"SMask"),
            _ => None,
        })
        .collect();
    let mut refs: Vec<(ObjRef, f64)> = ppi.into_iter().filter(|(r, _)| !masks.contains(r)).collect();
    refs.sort_by_key(|(r, _)| r.num);
    for (r, ppi) in refs {
        let obj = doc.get(r);
        let Object::Stream(s) = &*obj else { continue };
        report.images += 1;
        if !(crate::despeckle::MIN_DPI..=crate::despeckle::MAX_DPI).contains(&ppi) {
            report.images_skipped += 1;
            continue;
        }
        let done = if is_mono(doc, &s.dict) { despeckle_mono(s, ppi, options) } else { despeckle_colour(doc, s, ppi, options) };
        match done {
            Ok(Some((new, stats))) => {
                report.images_changed += 1;
                report.stats.add(&stats);
                doc.set(r, Object::Stream(new));
            }
            Ok(None) => {}
            Err(()) => report.images_skipped += 1,
        }
    }
    report
}

/// What [`crate::remove_page_image_backgrounds`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BackgroundReport {
    /// Images the pages draw (soft masks excluded).
    pub images: usize,
    /// Images rewritten because their background was whitened.
    pub images_changed: usize,
    /// Images left alone because PdfCraft can't decode or re-encode them faithfully (JBIG2,
    /// JPEG 2000, CMYK and other colour spaces, decode arrays, colour-key masks, more than 8
    /// bits) or they were too large.
    pub images_skipped: usize,
    /// Images decoded but left alone because they have no light paper background
    /// (photographs, dark or inverted scans) or it was already white. One-bit images are
    /// counted here: their background is already white.
    pub images_unchanged: usize,
    /// Pixels that became white, over all images.
    pub pixels_whitened: u64,
    /// Pixels whose value changed, over all images.
    pub pixels_changed: u64,
}

/// One 8-bit gray or RGB image with its background whitened (JPEG images are re-encoded at
/// quality 90, others with Flate); `Ok(None)` when nothing changed.
fn remove_background_colour(
    doc: &Document,
    s: &Stream,
    options: crate::background::Options,
) -> Result<Option<(Stream, crate::background::Stats)>, ()> {
    let d = &s.dict;
    if d.contains(b"Decode") || d.contains(b"Mask") || d.int(b"BitsPerComponent") != Some(8) {
        return Err(());
    }
    let n = d.get(b"ColorSpace").and_then(|cs| components(doc, cs)).ok_or(())?;
    let was_dct = is_dct(s).ok_or(())?;
    let (w, h) = (u32::try_from(d.int(b"Width").ok_or(())?).map_err(|_| ())?, u32::try_from(d.int(b"Height").ok_or(())?).map_err(|_| ())?);
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err(());
    }
    let need = (w as usize).checked_mul(h as usize).and_then(|p| p.checked_mul(n)).ok_or(())?;
    let mut px = if was_dct {
        pixels(s, n).ok_or(())?
    } else {
        // Lossless rewriting: the samples must decode completely (no truncated data published).
        s.decoded_complete(need.saturating_add(1 << 16)).map_err(|_| ())?.get(..need).ok_or(())?.to_vec()
    };
    if px.len() != need {
        return Err(());
    }
    let stats = crate::background::remove_background_pixels(&mut px, w, h, n, options).map_err(|_| ())?;
    if !stats.changed() {
        return Ok(None);
    }
    let compression = if was_dct { Compression::Jpeg(90) } else { Compression::Flate };
    Ok(Some((image_stream(d, &px, n, w, h, compression, was_dct).ok_or(())?, stats)))
}

pub(crate) fn remove_backgrounds(doc: &mut Document, pages: &[ObjRef], options: crate::background::Options) -> BackgroundReport {
    let mut report = BackgroundReport::default();
    let ppi = effective_resolutions(doc, pages);
    let masks: HashSet<ObjRef> = ppi
        .keys()
        .filter_map(|r| match &*doc.get(*r) {
            Object::Stream(s) => s.dict.reference(b"SMask"),
            _ => None,
        })
        .collect();
    let mut refs: Vec<ObjRef> = ppi.into_keys().filter(|r| !masks.contains(r)).collect();
    refs.sort_by_key(|r| (r.num, r.generation));
    for r in refs {
        let obj = doc.get(r);
        let Object::Stream(s) = &*obj else { continue };
        report.images += 1;
        if is_mono(doc, &s.dict) {
            report.images_unchanged += 1;
            continue;
        }
        match remove_background_colour(doc, s, options) {
            Ok(Some((new, stats))) => {
                report.images_changed += 1;
                report.pixels_whitened = report.pixels_whitened.saturating_add(stats.pixels_whitened);
                report.pixels_changed = report.pixels_changed.saturating_add(stats.pixels_changed);
                doc.set(r, Object::Stream(new));
            }
            Ok(None) => report.images_unchanged += 1,
            Err(()) => report.images_skipped += 1,
        }
    }
    report
}
