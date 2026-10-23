//! Graphics for a visible signature (Acrobat: Configure Signature Appearance ▸ Imported graphic /
//! Draw, and a logo behind the text): a picture or a hand-drawn signature on the left, and a
//! faint logo across the whole box. Everything here is input from files, settings or tools,
//! so sizes are checked when a graphic is made and again when it is drawn.

use pdfcraft_cos::{Dict, Document, ObjRef, Object, Stream};

use crate::SignError;

/// The largest picture side accepted (pixels); larger pictures are scaled down by the caller.
pub const MAX_IMAGE_SIDE: u32 = 2048;
/// The most strokes, and points in all, a drawn signature keeps.
pub const MAX_INK_STROKES: usize = 256;
pub const MAX_INK_POINTS: usize = 8192;
/// Drawn coordinates run from 0 to this on both axes (y down, as drawn on screen).
pub const INK_UNITS: u16 = 1000;
/// The logo's opacity behind the signature text.
const LOGO_OPACITY: f64 = 0.25;

/// A picture as 8-bit RGBA pixels (straight alpha), row by row from the top.
#[derive(Clone, PartialEq, Eq)]
pub struct GraphicImage {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl std::fmt::Debug for GraphicImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GraphicImage({}×{})", self.width, self.height)
    }
}

impl GraphicImage {
    /// Check the size and the pixel count against the buffer.
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self, SignError> {
        if width == 0 || height == 0 || width > MAX_IMAGE_SIDE || height > MAX_IMAGE_SIDE {
            return Err(SignError::Pdf(format!("a signature picture must be 1 to {MAX_IMAGE_SIDE} pixels on each side")));
        }
        let expected = (width as usize).checked_mul(height as usize).and_then(|n| n.checked_mul(4));
        if expected != Some(rgba.len()) {
            return Err(SignError::Pdf("the signature picture's pixels don't match its size".into()));
        }
        Ok(Self { width, height, rgba })
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }
}

/// A drawn signature: strokes of points in `0..=INK_UNITS` (y down).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InkGraphic {
    strokes: Vec<Vec<[u16; 2]>>,
}

impl InkGraphic {
    /// Keep strokes with at least one point, clamped into range; too many is an error.
    pub fn new(strokes: Vec<Vec<[u16; 2]>>) -> Result<Self, SignError> {
        let strokes: Vec<Vec<[u16; 2]>> = strokes
            .into_iter()
            .filter(|s| !s.is_empty())
            .map(|s| s.into_iter().map(|[x, y]| [x.min(INK_UNITS), y.min(INK_UNITS)]).collect())
            .collect();
        let points: usize = strokes.iter().map(Vec::len).sum();
        if strokes.is_empty() {
            return Err(SignError::Pdf("a drawn signature needs at least one stroke".into()));
        }
        if strokes.len() > MAX_INK_STROKES || points > MAX_INK_POINTS {
            return Err(SignError::Pdf(format!("a drawn signature has at most {MAX_INK_STROKES} strokes and {MAX_INK_POINTS} points")));
        }
        Ok(Self { strokes })
    }

    /// Strokes from any finite coordinates (screen points, a saved drawing): fitted into the
    /// unit box keeping their proportions.
    pub fn fit(strokes: &[Vec<[f32; 2]>]) -> Result<Self, SignError> {
        let all = || strokes.iter().flatten().filter(|p| p[0].is_finite() && p[1].is_finite());
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for p in all() {
            x0 = x0.min(p[0]);
            y0 = y0.min(p[1]);
            x1 = x1.max(p[0]);
            y1 = y1.max(p[1]);
        }
        if x0 > x1 {
            return Err(SignError::Pdf("a drawn signature needs at least one stroke".into()));
        }
        let span = (x1 - x0).max(y1 - y0).max(1e-3);
        let k = f32::from(INK_UNITS) / span;
        let unit = |v: f32| (v * k).round().clamp(0.0, f32::from(INK_UNITS)) as u16;
        let out = strokes
            .iter()
            .map(|s| s.iter().filter(|p| p[0].is_finite() && p[1].is_finite()).map(|p| [unit(p[0] - x0), unit(p[1] - y0)]).collect())
            .collect();
        Self::new(out)
    }

    pub fn strokes(&self) -> &[Vec<[u16; 2]>] {
        &self.strokes
    }

    /// The drawing's extent in units: (x0, y0, x1, y1).
    fn bounds(&self) -> [f64; 4] {
        let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        for p in self.strokes.iter().flatten() {
            let (x, y) = (f64::from(p[0]), f64::from(p[1]));
            b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
        }
        if b[0] > b[2] { [0.0; 4] } else { b }
    }
}

/// What the left side of a visible signature shows instead of the signer's name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignatureGraphic {
    /// An imported picture (a scanned signature, a stamp).
    Image(GraphicImage),
    /// A signature drawn with the mouse, pen or finger.
    Ink(InkGraphic),
}

/// Fit a `w`×`h` item into `area` (x, y, width, height; PDF space), centred, keeping its
/// proportions: the item's (x, y, width, height).
fn fit(area: [f64; 4], w: f64, h: f64) -> Option<[f64; 4]> {
    if !(w > 0.0 && h > 0.0 && area[2] > 0.0 && area[3] > 0.0) {
        return None;
    }
    let k = (area[2] / w).min(area[3] / h);
    let (fw, fh) = (w * k, h * k);
    Some([area[0] + (area[2] - fw) / 2.0, area[1] + (area[3] - fh) / 2.0, fw, fh])
}

fn num(v: f64) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.to_string() }
}

/// Add `img` to `doc` as an image XObject (with a soft mask when any pixel is translucent).
fn image_xobject(doc: &mut Document, img: &GraphicImage) -> ObjRef {
    let n = (img.width as usize).saturating_mul(img.height as usize);
    let mut rgb = Vec::with_capacity(n.saturating_mul(3));
    let mut alpha = Vec::with_capacity(n);
    let (pixels, _) = img.rgba.as_chunks::<4>();
    for px in pixels {
        rgb.extend_from_slice(&px[..3]);
        alpha.push(px[3]);
    }
    let base = |cs: &str| {
        let mut d = Dict::new();
        d.set(b"Type".to_vec(), Object::name("XObject"));
        d.set(b"Subtype".to_vec(), Object::name("Image"));
        d.set(b"Width".to_vec(), Object::Int(i64::from(img.width)));
        d.set(b"Height".to_vec(), Object::Int(i64::from(img.height)));
        d.set(b"ColorSpace".to_vec(), Object::name(cs));
        d.set(b"BitsPerComponent".to_vec(), Object::Int(8));
        d
    };
    let mut d = base("DeviceRGB");
    if alpha.iter().any(|a| *a < 255) {
        let mask = doc.add(Object::Stream(Stream::flate(base("DeviceGray"), &alpha)));
        d.set(b"SMask".to_vec(), Object::Ref(mask));
    }
    doc.add(Object::Stream(Stream::flate(d, &rgb)))
}

/// Draw the logo across the whole `w`×`h` box, faint, behind everything else. Adds the
/// resources it uses to `xobjects` / `gstates`.
pub(crate) fn draw_logo(doc: &mut Document, logo: &GraphicImage, w: f64, h: f64, xobjects: &mut Dict, gstates: &mut Dict) -> Vec<u8> {
    let Some([x, y, fw, fh]) = fit([0.0, 0.0, w, h], f64::from(logo.width), f64::from(logo.height)) else { return Vec::new() };
    let r = image_xobject(doc, logo);
    xobjects.set(b"Logo".to_vec(), Object::Ref(r));
    let mut gs = Dict::new();
    gs.set(b"Type".to_vec(), Object::name("ExtGState"));
    gs.set(b"ca".to_vec(), Object::Real(LOGO_OPACITY));
    gs.set(b"CA".to_vec(), Object::Real(LOGO_OPACITY));
    gstates.set(b"LogoGS".to_vec(), Object::Dict(gs));
    format!("q /LogoGS gs {} 0 0 {} {} {} cm /Logo Do Q\n", num(fw), num(fh), num(x), num(y)).into_bytes()
}

/// Draw `g` fitted into `area` (x, y, width, height).
pub(crate) fn draw_graphic(doc: &mut Document, g: &SignatureGraphic, area: [f64; 4], xobjects: &mut Dict) -> Vec<u8> {
    match g {
        SignatureGraphic::Image(img) => {
            let Some([x, y, fw, fh]) = fit(area, f64::from(img.width), f64::from(img.height)) else { return Vec::new() };
            let r = image_xobject(doc, img);
            xobjects.set(b"Graphic".to_vec(), Object::Ref(r));
            format!("q {} 0 0 {} {} {} cm /Graphic Do Q\n", num(fw), num(fh), num(x), num(y)).into_bytes()
        }
        SignatureGraphic::Ink(ink) => {
            let b = ink.bounds();
            // A dot or a straight line still gets a size to fit.
            let (bw, bh) = ((b[2] - b[0]).max(1.0), (b[3] - b[1]).max(1.0));
            let Some([x, y, fw, fh]) = fit(area, bw, bh) else { return Vec::new() };
            let k = fw / bw;
            let width = (fh * 0.04).clamp(0.5, 3.0);
            let mut out = format!("q 0.08 0.1 0.32 RG {} w 1 J 1 j\n", num(width)).into_bytes();
            for s in ink.strokes() {
                let pt = |p: &[u16; 2]| (x + (f64::from(p[0]) - b[0]) * k, y + fh - (f64::from(p[1]) - b[1]) * k);
                let Some(first) = s.first() else { continue };
                let (sx, sy) = pt(first);
                out.extend(format!("{} {} m\n", num(sx), num(sy)).bytes());
                if s.len() == 1 {
                    // A dot: a zero-length segment with round caps.
                    out.extend(format!("{} {} l\n", num(sx), num(sy)).bytes());
                }
                for p in s.iter().skip(1) {
                    let (px, py) = pt(p);
                    out.extend(format!("{} {} l\n", num(px), num(py)).bytes());
                }
                out.extend_from_slice(b"S\n");
            }
            out.extend_from_slice(b"Q\n");
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_sizes_are_refused() {
        assert!(GraphicImage::new(0, 1, vec![]).is_err());
        assert!(GraphicImage::new(MAX_IMAGE_SIDE + 1, 1, vec![0; (MAX_IMAGE_SIDE as usize + 1) * 4]).is_err());
        assert!(GraphicImage::new(2, 2, vec![0; 15]).is_err());
        assert!(GraphicImage::new(u32::MAX, u32::MAX, vec![]).is_err());
        assert!(GraphicImage::new(2, 2, vec![0; 16]).is_ok());
        assert!(InkGraphic::new(vec![]).is_err());
        assert!(InkGraphic::new(vec![vec![]; 10]).is_err());
        assert!(InkGraphic::new(vec![vec![[0, 0]]; MAX_INK_STROKES + 1]).is_err());
        assert!(InkGraphic::new(vec![vec![[0, 0]; MAX_INK_POINTS + 1]]).is_err());
        assert_eq!(InkGraphic::new(vec![vec![[u16::MAX, 5]]]).unwrap().strokes()[0][0], [INK_UNITS, 5]);
        assert!(InkGraphic::fit(&[vec![[f32::NAN, f32::INFINITY]]]).is_err());
        let fitted = InkGraphic::fit(&[vec![[10.0, 10.0], [f32::NAN, 0.0], [110.0, 60.0]]]).unwrap();
        assert_eq!(fitted.strokes()[0], vec![[0, 0], [1000, 500]]);
    }

    #[test]
    fn a_tiny_or_empty_area_draws_nothing() {
        let mut doc = Document::new_empty();
        let mut x = Dict::new();
        let ink = SignatureGraphic::Ink(InkGraphic::new(vec![vec![[5, 5]]]).unwrap());
        assert!(draw_graphic(&mut doc, &ink, [0.0, 0.0, 0.0, 10.0], &mut x).is_empty());
        let dot = draw_graphic(&mut doc, &ink, [0.0, 0.0, 10.0, 10.0], &mut x);
        assert!(String::from_utf8(dot).unwrap().contains(" l\nS"));
    }
}
