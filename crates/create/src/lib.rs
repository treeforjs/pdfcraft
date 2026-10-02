//! printcraft-create — create PDFs from nothing, images or text (L4). See the README.

use printcraft_cos::{Dict, Document, ObjRef, Object, PdfString, Stream};
use printcraft_fonts::{literal, win_ansi, wrap};

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum CreateError {
    #[error("{0}: {1}")]
    Image(String, String),
    #[error("{0}")]
    Invalid(String),
}

/// US Letter in points.
pub const LETTER: (f64, f64) = (612.0, 792.0);

/// The largest page side PDF allows (ISO 32000-2 Annex C: 14 400 units).
const MAX_SIDE: f64 = 14_400.0;

fn add_page(doc: &mut Document, w: f64, h: f64, resources: Dict, content: Option<Vec<u8>>) -> ObjRef {
    let pages = doc.root().and_then(|r| doc.get(r).as_dict().and_then(|d| d.reference(b"Pages"))).expect("new_empty has a page tree");
    let mut page = Dict::new();
    page.set(b"Type".to_vec(), Object::name("Page"));
    page.set(b"Parent".to_vec(), Object::Ref(pages));
    page.set(b"MediaBox".to_vec(), Object::Array(vec![0.into(), 0.into(), Object::Real(w), Object::Real(h)]));
    page.set(b"Resources".to_vec(), Object::Dict(resources));
    if let Some(c) = content {
        let s = doc.add(Object::Stream(Stream::flate(Dict::new(), &c)));
        page.set(b"Contents".to_vec(), Object::Ref(s));
    }
    let r = doc.add(page);
    let _ = doc.update_dict(pages, |d| {
        let mut kids = d.get(b"Kids").and_then(|k| k.as_array().cloned()).unwrap_or_default();
        kids.push(Object::Ref(r));
        d.set(b"Count".to_vec(), Object::Int(kids.len() as i64));
        d.set(b"Kids".to_vec(), Object::Array(kids));
    });
    r
}

fn set_title(doc: &mut Document, title: &str) {
    let mut info = Dict::new();
    info.set(b"Title".to_vec(), PdfString::text(title));
    info.set(b"Producer".to_vec(), PdfString::text("PrintCraft"));
    let r = doc.add(info);
    doc.trailer_mut().set(b"Info".to_vec(), Object::Ref(r));
}

/// A document of `pages` empty pages of `width × height` points.
pub fn blank(width: f64, height: f64, pages: usize) -> Result<Document, CreateError> {
    if !(width.is_finite() && height.is_finite() && (3.0..=MAX_SIDE).contains(&width) && (3.0..=MAX_SIDE).contains(&height))
        || pages == 0
        || pages > 10_000
    {
        return Err(CreateError::Invalid("invalid page size or count".into()));
    }
    let mut doc = Document::new_empty();
    for _ in 0..pages {
        add_page(&mut doc, width, height, Dict::new(), None);
    }
    Ok(doc)
}

// ── images ──────────────────────────────────────────────────────────────────────────────────

/// An image ready to embed: its XObject dictionary, encoded data, optional soft mask, and size
/// in pixels and dots per inch.
struct Embedded {
    dict: Dict,
    data: Vec<u8>,
    filtered: bool,
    smask: Option<(Dict, Vec<u8>)>,
    px: (u32, u32),
    dpi: (f64, f64),
}

fn jpeg(name: &str, bytes: &[u8]) -> Result<Embedded, CreateError> {
    let bad = |m: &str| CreateError::Image(name.into(), m.into());
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return Err(bad("not a JPEG file"));
    }
    let (mut i, mut size, mut comps, mut dpi, mut adobe) = (2usize, None, 0u8, (72.0, 72.0), false);
    while i + 4 <= bytes.len() {
        if bytes[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = bytes[i + 1];
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
        let seg = bytes.get(i + 4..i + 2 + len).ok_or_else(|| bad("truncated"))?;
        match marker {
            // APP0 JFIF density.
            0xE0 if seg.starts_with(b"JFIF\0") && seg.len() >= 12 => {
                let (unit, x, y) = (seg[7], u16::from_be_bytes([seg[8], seg[9]]) as f64, u16::from_be_bytes([seg[10], seg[11]]) as f64);
                if x > 0.0 && y > 0.0 {
                    dpi = match unit {
                        1 => (x, y),
                        2 => (x * 2.54, y * 2.54),
                        _ => dpi,
                    };
                }
            }
            0xEE if seg.starts_with(b"Adobe") => adobe = true,
            0xC0..=0xCF if marker != 0xC4 && marker != 0xC8 && marker != 0xCC => {
                if seg.len() < 6 {
                    return Err(bad("bad frame header"));
                }
                size = Some((u16::from_be_bytes([seg[3], seg[4]]) as u32, u16::from_be_bytes([seg[1], seg[2]]) as u32));
                comps = seg[5];
                break;
            }
            0xDA => break,
            _ => {}
        }
        i += 2 + len;
    }
    let (w, h) = size.filter(|(w, h)| *w > 0 && *h > 0).ok_or_else(|| bad("no image size"))?;
    let mut d = Dict::new();
    let cs = match comps {
        1 => "DeviceGray",
        3 => "DeviceRGB",
        4 => {
            if adobe {
                // Adobe writes CMYK JPEGs inverted.
                d.set(b"Decode".to_vec(), Object::Array([1, 0, 1, 0, 1, 0, 1, 0].iter().map(|v| Object::Int(*v)).collect()));
            }
            "DeviceCMYK"
        }
        n => return Err(bad(&format!("{n} colour components are not supported"))),
    };
    d.set(b"ColorSpace".to_vec(), Object::name(cs));
    d.set(b"BitsPerComponent".to_vec(), Object::Int(8));
    d.set(b"Filter".to_vec(), Object::name("DCTDecode"));
    Ok(Embedded { dict: d, data: bytes.to_vec(), filtered: true, smask: None, px: (w, h), dpi })
}

fn png_image(name: &str, bytes: &[u8]) -> Result<Embedded, CreateError> {
    let bad = |m: String| CreateError::Image(name.into(), m);
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().map_err(|e| bad(e.to_string()))?;
    let dpi = match reader.info().pixel_dims {
        Some(png::PixelDimensions { xppu, yppu, unit: png::Unit::Meter }) if xppu > 0 && yppu > 0 => (xppu as f64 * 0.0254, yppu as f64 * 0.0254),
        _ => (72.0, 72.0),
    };
    let mut buf = vec![0; reader.output_buffer_size().ok_or_else(|| bad("image too large".into()))?];
    let frame = reader.next_frame(&mut buf).map_err(|e| bad(e.to_string()))?;
    let (w, h) = (frame.width, frame.height);
    let data = &buf[..frame.buffer_size()];
    let (channels, cs) = match frame.color_type {
        png::ColorType::Grayscale => (1, "DeviceGray"),
        png::ColorType::GrayscaleAlpha => (2, "DeviceGray"),
        png::ColorType::Rgb => (3, "DeviceRGB"),
        png::ColorType::Rgba => (4, "DeviceRGB"),
        png::ColorType::Indexed => return Err(bad("unexpected palette output".into())),
    };
    let has_alpha = channels == 2 || channels == 4;
    let colour_n = if has_alpha { channels - 1 } else { channels };
    let mut colour = Vec::with_capacity(w as usize * h as usize * colour_n);
    let mut alpha = Vec::new();
    for px in data.chunks_exact(channels) {
        colour.extend_from_slice(&px[..colour_n]);
        if has_alpha {
            alpha.push(px[colour_n]);
        }
    }
    let mut d = Dict::new();
    d.set(b"ColorSpace".to_vec(), Object::name(cs));
    d.set(b"BitsPerComponent".to_vec(), Object::Int(8));
    let smask = (has_alpha && alpha.iter().any(|a| *a != 255)).then(|| {
        let mut m = Dict::new();
        m.set(b"Type".to_vec(), Object::name("XObject"));
        m.set(b"Subtype".to_vec(), Object::name("Image"));
        m.set(b"Width".to_vec(), Object::Int(w as i64));
        m.set(b"Height".to_vec(), Object::Int(h as i64));
        m.set(b"ColorSpace".to_vec(), Object::name("DeviceGray"));
        m.set(b"BitsPerComponent".to_vec(), Object::Int(8));
        (m, alpha)
    });
    Ok(Embedded { dict: d, data: colour, filtered: false, smask, px: (w, h), dpi })
}

/// Detect the image format from its bytes.
fn embed(name: &str, bytes: &[u8]) -> Result<Embedded, CreateError> {
    if bytes.starts_with(&[0xFF, 0xD8]) {
        jpeg(name, bytes)
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        png_image(name, bytes)
    } else {
        Err(CreateError::Image(name.into(), "only PNG and JPEG images are supported so far".into()))
    }
}

/// One page per image, each the size of its image at the image's resolution.
pub fn from_images(images: &[(String, Vec<u8>)]) -> Result<Document, CreateError> {
    if images.is_empty() {
        return Err(CreateError::Invalid("no images".into()));
    }
    let mut doc = Document::new_empty();
    for (name, bytes) in images {
        let img = embed(name, bytes)?;
        let (mut w, mut h) = (img.px.0 as f64 * 72.0 / img.dpi.0, img.px.1 as f64 * 72.0 / img.dpi.1);
        // Keep huge images within the largest page PDF allows.
        let k = (MAX_SIDE / w.max(h)).min(1.0);
        w *= k;
        h *= k;
        let mut d = img.dict;
        d.set(b"Type".to_vec(), Object::name("XObject"));
        d.set(b"Subtype".to_vec(), Object::name("Image"));
        d.set(b"Width".to_vec(), Object::Int(img.px.0 as i64));
        d.set(b"Height".to_vec(), Object::Int(img.px.1 as i64));
        if let Some((m, alpha)) = img.smask {
            let mr = doc.add(Object::Stream(Stream::flate(m, &alpha)));
            d.set(b"SMask".to_vec(), Object::Ref(mr));
        }
        let stream = if img.filtered { Stream::from_raw(d, img.data) } else { Stream::flate(d, &img.data) };
        let xr = doc.add(Object::Stream(stream));
        let mut xobj = Dict::new();
        xobj.set(b"Im0".to_vec(), Object::Ref(xr));
        let mut res = Dict::new();
        res.set(b"XObject".to_vec(), Object::Dict(xobj));
        let content = format!("q {w:.3} 0 0 {h:.3} 0 0 cm /Im0 Do Q\n").into_bytes();
        add_page(&mut doc, w, h, res, Some(content));
    }
    if let Some((name, _)) = images.first() {
        set_title(&mut doc, name.rsplit_once('.').map_or(name.as_str(), |(s, _)| s));
    }
    Ok(doc)
}

// ── text ────────────────────────────────────────────────────────────────────────────────────

/// Plain text set in Helvetica on pages of `page` size with 1-inch margins.
pub fn from_text(title: &str, text: &str, page: (f64, f64), font_size: f64) -> Result<Document, CreateError> {
    let size = if font_size.is_finite() && font_size > 0.0 { font_size.clamp(4.0, 72.0) } else { 11.0 };
    let (w, h) = page;
    let margin = 72.0;
    let line_h = size * 1.25;
    let width = (w - 2.0 * margin).max(36.0);
    let lines: Vec<String> = text
        .replace("\r\n", "\n")
        .replace('\t', "    ")
        .split('\u{c}')
        .flat_map(|p| wrap(p, size, width).into_iter().chain(std::iter::once("\u{c}".to_string())))
        .collect();
    let per_page = (((h - 2.0 * margin) / line_h).floor() as usize).max(1);
    let mut doc = Document::new_empty();
    let mut font = Dict::new();
    font.set(b"Type".to_vec(), Object::name("Font"));
    font.set(b"Subtype".to_vec(), Object::name("Type1"));
    font.set(b"BaseFont".to_vec(), Object::name("Helvetica"));
    font.set(b"Encoding".to_vec(), Object::name("WinAnsiEncoding"));
    let fr = doc.add(font);
    let mut page_lines: Vec<Vec<String>> = vec![Vec::new()];
    for l in lines.iter().take(lines.len().saturating_sub(1)) {
        if l == "\u{c}" || page_lines.last().is_some_and(|p| p.len() >= per_page) {
            page_lines.push(Vec::new());
            if l == "\u{c}" {
                continue;
            }
        }
        page_lines.last_mut().expect("non-empty").push(l.clone());
    }
    for lines in page_lines {
        let mut c = format!("BT /F1 {size} Tf {line_h:.3} TL {margin} {:.3} Td\n", h - margin - size).into_bytes();
        for l in lines {
            c.extend(literal(&win_ansi(&l)));
            c.extend_from_slice(b" Tj T*\n");
        }
        c.extend_from_slice(b"ET\n");
        let mut fonts = Dict::new();
        fonts.set(b"F1".to_vec(), Object::Ref(fr));
        let mut res = Dict::new();
        res.set(b"Font".to_vec(), Object::Dict(fonts));
        add_page(&mut doc, w, h, res, Some(c));
    }
    set_title(&mut doc, title);
    Ok(doc)
}

#[cfg(test)]
mod tests;
