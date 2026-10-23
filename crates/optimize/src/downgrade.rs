//! Make compatible with: image features an older PDF version doesn't define, converted down.
//!
//! - **JPEG 2000** (PDF 1.5): decoded with `hayro-jpeg2000` and stored as JPEG (gray and RGB,
//!   at the colour or grayscale images' JPEG quality, else 80) or Flate (CMYK, or when the
//!   images are set to ZIP). Alpha the image carries itself (`/SMaskInData`) becomes a soft
//!   mask; an image without `/ColorSpace` gets the space its codestream declares (an embedded
//!   ICC profile becomes an `ICCBased` space).
//! - **16-bit images** (PDF 1.5): reduced to 8 bits per component (the high byte, so `/Decode`
//!   keeps its meaning).
//! - **JBIG2** (PDF 1.4): decoded with `hayro-jbig2` (with its `/JBIG2Globals`) and stored as
//!   CCITT Group 4, with the same samples.
//!
//! An image is only replaced when it decodes completely within the pixel limit; anything else
//! is left as it was and still reported by [`crate::newer_features`].

use pdfcraft_cos::{Dict, Document, ObjRef, Object, Stream};

use crate::images::MAX_PIXELS;
use crate::{Compression, Report, Settings};

/// Convert what `target` doesn't define in every image of `doc`.
pub(crate) fn run(doc: &mut Document, target: (u8, u8), settings: &Settings, report: &mut Report) {
    for num in doc.object_numbers() {
        let r = ObjRef { num, generation: doc.generation(num) };
        let obj = doc.get(r);
        let Object::Stream(s) = &*obj else { continue };
        if s.dict.name(b"Subtype") != Some(b"Image".as_slice()) {
            continue;
        }
        let filter = single_filter(&s.dict);
        let converted = if target < (1, 5) && filter.as_deref() == Some(b"JPXDecode".as_slice()) {
            jpx(doc, s, settings)
        } else if target < (1, 4) && filter.as_deref() == Some(b"JBIG2Decode".as_slice()) {
            jbig2(doc, s).map(|image| Converted { image, smask: None, icc: None })
        } else if target < (1, 5) && s.dict.int(b"BitsPerComponent") == Some(16) {
            sixteen_bit(doc, s).map(|image| Converted { image, smask: None, icc: None })
        } else {
            continue;
        };
        let Some(Converted { mut image, smask, icc }) = converted else {
            report.images_not_downgraded += 1;
            continue;
        };
        if let Some((icc, _)) = icc {
            let ir = doc.add(Object::Stream(icc));
            image.dict.set(b"ColorSpace".to_vec(), Object::Array(vec![Object::name("ICCBased"), Object::Ref(ir)]));
        }
        if let Some(m) = smask {
            let mr = doc.add(Object::Stream(m));
            image.dict.set(b"SMask".to_vec(), Object::Ref(mr));
        }
        doc.set(r, Object::Stream(image));
        report.images_downgraded += 1;
    }
}

/// A converted image, its new soft mask, and the ICC profile its colour space needs.
struct Converted {
    image: Stream,
    smask: Option<Stream>,
    icc: Option<(Stream, u8)>,
}

/// The stream's only filter, when it has exactly one (direct).
fn single_filter(d: &Dict) -> Option<Vec<u8>> {
    match d.get(b"Filter")? {
        Object::Name(n) => Some(n.clone()),
        Object::Array(a) if a.len() == 1 => a.first()?.as_name().map(<[u8]>::to_vec),
        _ => None,
    }
}

/// The decode parameters of a single-filter stream.
fn parms(d: &Dict) -> Option<Dict> {
    match d.get(b"DecodeParms") {
        Some(Object::Dict(p)) => Some(p.clone()),
        Some(Object::Array(a)) => a.first().and_then(|p| p.as_dict().cloned()),
        _ => None,
    }
}

fn bare(d: &Dict) -> Dict {
    let mut d = d.clone();
    for k in [&b"Filter"[..], b"DecodeParms", b"Length", b"DL", b"SMaskInData"] {
        d.remove(k);
    }
    d
}

/// A JPEG 2000 image as JPEG or Flate, and its alpha as a soft mask when it carries one.
fn jpx(doc: &Document, s: &Stream, settings: &Settings) -> Option<Converted> {
    if s.dict.contains(b"F") || matches!(s.dict.get(b"ImageMask"), Some(Object::Bool(true))) {
        return None;
    }
    let image = crate::guarded(|| hayro_jpeg2000::Image::new(&s.raw, &hayro_jpeg2000::DecodeSettings::default()).ok())??;
    let (w, h) = (image.width(), image.height());
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_PIXELS {
        return None;
    }
    let channels = usize::from(image.color_space().num_channels());
    let alpha = image.has_alpha();
    let stride = channels + usize::from(alpha);
    let declared = s.dict.get(b"ColorSpace").cloned();
    let mut icc = None;
    let space = match &declared {
        Some(cs) => {
            // The PDF colour space wins (ISO 32000-2 §8.9.5.2); it must have as many components.
            if component_count(doc, cs)? != channels {
                return None;
            }
            None
        }
        None => match image.color_space() {
            hayro_jpeg2000::ColorSpace::Gray => Some(Object::name("DeviceGray")),
            hayro_jpeg2000::ColorSpace::RGB => Some(Object::name("DeviceRGB")),
            hayro_jpeg2000::ColorSpace::CMYK => Some(Object::name("DeviceCMYK")),
            hayro_jpeg2000::ColorSpace::Icc { profile, num_channels } if matches!(num_channels, 1 | 3 | 4) => {
                let mut d = Dict::new();
                d.set(b"N".to_vec(), Object::Int(i64::from(*num_channels)));
                icc = Some((Stream::flate(d, profile), *num_channels));
                None
            }
            _ => return None,
        },
    };
    let decoded = crate::guarded(|| image.decode().ok())??;
    let pixels = usize::try_from(u64::from(w) * u64::from(h)).ok()?;
    if decoded.len() != pixels.checked_mul(stride)? {
        return None;
    }
    let (colour, mask): (Vec<u8>, Option<Vec<u8>>) = if alpha {
        let mut c = Vec::with_capacity(pixels * channels);
        let mut a = Vec::with_capacity(pixels);
        for px in decoded.chunks_exact(stride) {
            c.extend_from_slice(px.get(..channels)?);
            a.push(*px.last()?);
        }
        (c, (s.dict.int(b"SMaskInData").is_some_and(|v| v != 0) && !s.dict.contains(b"SMask")).then_some(a))
    } else {
        (decoded, None)
    };
    let mut d = bare(&s.dict);
    // JPEG 2000 ignores /Decode (except for masks); the 8-bit copy must too.
    d.remove(b"Decode");
    if let Some(cs) = space {
        d.set(b"ColorSpace".to_vec(), cs);
    }
    let st = if channels == 1 { settings.gray } else { settings.color };
    let compression = match st.compression {
        Compression::Jpeg(q) => Compression::Jpeg(q),
        Compression::Flate => Compression::Flate,
        Compression::Retain => Compression::Jpeg(80),
    };
    let compression = if channels == 4 { Compression::Flate } else { compression };
    let image = crate::images::image_stream(&d, &colour, channels, w, h, compression, false)?;
    let smask = match mask {
        Some(a) => {
            let mut m = Dict::new();
            m.set(b"Type".to_vec(), Object::name("XObject"));
            m.set(b"Subtype".to_vec(), Object::name("Image"));
            m.set(b"ColorSpace".to_vec(), Object::name("DeviceGray"));
            crate::images::image_stream(&m, &a, 1, w, h, Compression::Flate, false)
        }
        None => None,
    };
    Some(Converted { image, smask, icc })
}

/// Components of a colour space, for checking a JPEG 2000 image's samples against it.
fn component_count(doc: &Document, cs: &Object) -> Option<usize> {
    let cs = doc.resolve(cs);
    match &*cs {
        Object::Name(n) => match n.as_slice() {
            b"DeviceGray" | b"G" | b"CalGray" => Some(1),
            b"DeviceRGB" | b"RGB" | b"CalRGB" | b"Lab" => Some(3),
            b"DeviceCMYK" | b"CMYK" => Some(4),
            _ => None,
        },
        Object::Array(a) => match a.first()?.as_name()? {
            b"ICCBased" => match &*doc.resolve(a.get(1)?) {
                Object::Stream(icc) => usize::try_from(icc.dict.int(b"N")?).ok().filter(|n| matches!(n, 1 | 3 | 4)),
                _ => None,
            },
            b"CalGray" => Some(1),
            b"CalRGB" | b"Lab" => Some(3),
            b"Separation" => Some(1),
            b"DeviceN" => a.get(1).map(|n| doc.resolve(n)).and_then(|n| n.as_array().map(Vec::len)).filter(|n| (1..=32).contains(n)),
            b"Indexed" => Some(1),
            _ => None,
        },
        _ => None,
    }
}

/// A JBIG2 image as CCITT Group 4 with the same samples.
fn jbig2(doc: &Document, s: &Stream) -> Option<Stream> {
    if s.dict.contains(b"F") {
        return None;
    }
    let globals = match parms(&s.dict).and_then(|p| p.get(b"JBIG2Globals").cloned()) {
        Some(g) => match &*doc.resolve(&g) {
            Object::Stream(g) => Some(g.decoded_complete(16 << 20).ok()?),
            _ => return None,
        },
        None => None,
    };
    let (w, h) = (usize::try_from(s.dict.int(b"Width")?).ok()?, usize::try_from(s.dict.int(b"Height")?).ok()?);
    if w == 0 || h == 0 || w > 1 << 20 || (w as u64).saturating_mul(h as u64) > MAX_PIXELS {
        return None;
    }
    let bits = crate::guarded(|| decode_jbig2(&s.raw, globals.as_deref(), w, h))??;
    let mut d = bare(&s.dict);
    d.remove(b"JBIG2Globals");
    crate::images::mono_stream(&d, &bits, w, h, Some(Dict::new())).ok()
}

/// Decode a JBIG2 stream to packed PDF samples (0 = black), checking its size.
fn decode_jbig2(data: &[u8], globals: Option<&[u8]>, w: usize, h: usize) -> Option<Vec<u8>> {
    struct Sink {
        stride: usize,
        width: usize,
        x: usize,
        y: usize,
        out: Vec<u8>,
    }
    impl Sink {
        fn put(&mut self, black: bool) {
            if self.x < self.width
                && !black
                && let Some(b) = self.out.get_mut(self.y * self.stride + self.x / 8)
            {
                *b |= 0x80 >> (self.x % 8);
            }
            self.x += 1;
        }
    }
    impl hayro_jbig2::Decoder for Sink {
        fn push_pixel(&mut self, black: bool) {
            self.put(black);
        }
        fn push_pixel_chunk(&mut self, black: bool, chunk_count: u32) {
            for _ in 0..(chunk_count as usize).saturating_mul(8).min(self.width.saturating_add(8)) {
                self.put(black);
            }
        }
        fn next_line(&mut self) {
            self.x = 0;
            self.y += 1;
        }
    }
    let image = hayro_jbig2::Image::new_embedded(data, globals).ok()?;
    if usize::try_from(image.width()).ok()? != w || usize::try_from(image.height()).ok()? != h {
        return None;
    }
    let stride = w.div_ceil(8);
    let mut sink = Sink { stride, width: w, x: 0, y: 0, out: vec![0; stride.checked_mul(h)?] };
    image.decode(&mut sink).ok()?;
    (sink.y >= h).then_some(sink.out)
}

/// A 16-bit image reduced to 8 bits per component.
fn sixteen_bit(doc: &Document, s: &Stream) -> Option<Stream> {
    let d = &s.dict;
    // A colour-key mask lists 16-bit values; leave those alone.
    if matches!(d.get(b"Mask"), Some(Object::Array(_))) || d.contains(b"F") {
        return None;
    }
    let n = component_count(doc, d.get(b"ColorSpace")?)?;
    let (w, h) = (usize::try_from(d.int(b"Width")?).ok()?, usize::try_from(d.int(b"Height")?).ok()?);
    if w == 0 || h == 0 || (w as u64).saturating_mul(h as u64) > MAX_PIXELS {
        return None;
    }
    let row = w.checked_mul(n)?.checked_mul(2)?;
    let need = row.checked_mul(h)?;
    let data = s.decoded_complete(need.saturating_add(1 << 16)).ok()?;
    let data = data.get(..need)?;
    let (pairs, _) = data.as_chunks::<2>();
    let high: Vec<u8> = pairs.iter().map(|[hi, _]| *hi).collect();
    let mut nd = bare(d);
    nd.set(b"BitsPerComponent".to_vec(), Object::Int(8));
    Some(Stream::flate(nd, &high))
}
