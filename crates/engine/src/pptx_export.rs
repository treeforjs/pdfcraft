//! Export a PDF ▸ Microsoft PowerPoint (.pptx): one slide per page.
//!
//! [`PptxMode::Editable`] (the default) rebuilds each page from its parts: every paragraph is a
//! text box (font family, size, bold, italic and colour from the PDF, placed and turned as it
//! appears on the displayed page, `/Rotate` included), every image XObject that can be
//! extracted is a picture, and everything else is one rendered background picture under them:
//! the page drawn without its text and without those images (vector art, shadings, inline
//! images, form fields and comment appearances). Vector shapes are not rebuilt as PowerPoint
//! shapes; a page whose background renders blank gets none. [`PptxMode::Image`] makes every
//! slide one picture of its page.

use std::collections::HashSet;
use std::sync::Arc;

use pdfcraft_export::{Placement, PptxStats, Slide, SlidePicture, SlideText};
use pdfcraft_render::{PageInfo, PageRenderer, RenderRequest};

use crate::Document;
use crate::export::encode_png;

/// How pages become slides.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PptxMode {
    /// Text boxes, pictures and a background picture of the rest.
    #[default]
    Editable,
    /// Each slide is one picture of its page.
    Image,
}

impl PptxMode {
    pub fn from_id(id: &str) -> Option<PptxMode> {
        match id {
            "editable" => Some(PptxMode::Editable),
            "image" => Some(PptxMode::Image),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            PptxMode::Editable => "editable",
            PptxMode::Image => "image",
        }
    }
}

/// Export to PowerPoint options.
#[derive(Clone, Debug, PartialEq)]
pub struct PptxOptions {
    /// 0-based pages, in slide order; empty exports every page.
    pub pages: Vec<usize>,
    pub mode: PptxMode,
    /// Resolution of the rendered pictures (backgrounds, or whole pages in image mode), clamped
    /// to 36–600.
    pub dpi: f64,
}

impl Default for PptxOptions {
    fn default() -> Self {
        Self { pages: Vec::new(), mode: PptxMode::Editable, dpi: 150.0 }
    }
}

/// The presentation and what went into it.
#[derive(Clone, Debug, PartialEq)]
pub struct PptxExport {
    pub bytes: Vec<u8>,
    pub stats: PptxStats,
    /// Pages that could not be fully converted (a page that failed to render, slides past the
    /// limit).
    pub warnings: Vec<String>,
}

/// User space → displayed page (points, origin top left, y down, after `/Rotate`).
struct View {
    crop: [f64; 4],
    rotation: u16,
    width: f64,
    height: f64,
}

impl View {
    fn of(info: &PageInfo) -> View {
        let c = info.crop.map(f64::from);
        View { crop: c, rotation: info.rotation, width: f64::from(info.width), height: f64::from(info.height) }
    }

    fn point(&self, x: f64, y: f64) -> (f64, f64) {
        let [x0, y0, x1, y1] = self.crop;
        let (cw, ch) = ((x1 - x0).abs().max(1e-6), (y1 - y0).abs().max(1e-6));
        let (u, v) = ((x - x0) / cw, (y1 - y) / ch);
        let (a, b) = match self.rotation {
            90 => (1.0 - v, u),
            180 => (1.0 - u, 1.0 - v),
            270 => (v, 1.0 - u),
            _ => (u, v),
        };
        (a * self.width, b * self.height)
    }

    /// Displayed points per user-space unit (`/UserUnit`).
    fn scale(&self) -> f64 {
        let [x0, y0, x1, y1] = self.crop;
        let across = if matches!(self.rotation, 90 | 270) { (y1 - y0).abs() } else { (x1 - x0).abs() };
        if across > 1e-6 && self.width.is_finite() { self.width / across } else { 1.0 }
    }

    /// A box `w` × `h` (user units) centred on user point `centre`, its x axis at `angle`
    /// degrees counterclockwise in user space.
    fn place(&self, centre: [f64; 2], w: f64, h: f64, angle: f64, flip_v: bool) -> Placement {
        let (cx, cy) = self.point(centre[0], centre[1]);
        let s = self.scale();
        let (w, h) = (w * s, h * s);
        Placement { x: cx - w / 2.0, y: cy - h / 2.0, w, h, rot: f64::from(self.rotation) - angle, flip_v }
    }
}

/// Whether a rendered page is empty paper: every pixel white or transparent.
fn blank(rgba: &[u8]) -> bool {
    rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 0 || (p[3] == 255 && p[0] >= 250 && p[1] >= 250 && p[2] >= 250))
}

/// Render `page` of `renderer` as a PNG, or why it couldn't be.
fn render_png(renderer: &mut PageRenderer, page: usize, dpi: f64, skip_blank: bool) -> Result<Option<Vec<u8>>, String> {
    let r = renderer.render(RenderRequest { page, scale: (dpi / 72.0) as f32, ..Default::default() });
    if let Some(e) = r.error {
        return Err(format!("page {}: {e}", page + 1));
    }
    if skip_blank && blank(&r.rgba) {
        return Ok(None);
    }
    encode_png(r.width, r.height, &r.rgba).map(Some)
}

impl Document {
    /// The document as a PowerPoint presentation (see the module notes). Editable mode hands out
    /// the text and images, so it is refused when the document does not allow copying; image
    /// mode only renders pages, as Export ▸ Image does.
    pub fn export_pptx(&self, options: &PptxOptions) -> Result<PptxExport, String> {
        let (slides, warnings) = self.pptx_slides(options)?;
        let title = self.info.title.clone().unwrap_or_else(|| self.name.trim_end_matches(".pdf").to_string());
        let (bytes, stats) = pdfcraft_export::pptx(&slides, &title);
        Ok(PptxExport { bytes, stats, warnings })
    }

    /// The slides [`Document::export_pptx`] writes, and its warnings.
    pub fn pptx_slides(&self, options: &PptxOptions) -> Result<(Vec<Slide>, Vec<String>), String> {
        if options.mode == PptxMode::Editable {
            self.require_copying()?;
        }
        let n = self.info.pages.len();
        let mut pages: Vec<usize> = if options.pages.is_empty() { (0..n).collect() } else { options.pages.clone() };
        if let Some(p) = pages.iter().find(|p| **p >= n) {
            return Err(format!("page {} does not exist (the document has {n} pages)", p.saturating_add(1)));
        }
        let mut warnings = Vec::new();
        if pages.len() > pdfcraft_export::MAX_SLIDES {
            warnings.push(format!("only the first {} pages were exported", pdfcraft_export::MAX_SLIDES));
            pages.truncate(pdfcraft_export::MAX_SLIDES);
        }
        let dpi = if options.dpi.is_finite() { options.dpi.clamp(36.0, 600.0) } else { 150.0 };
        let mut slides: Vec<Slide> = pages
            .iter()
            .map(|&p| {
                let info = self.info.pages.get(p);
                Slide { width: info.map_or(612.0, |i| f64::from(i.width)), height: info.map_or(792.0, |i| f64::from(i.height)), ..Default::default() }
            })
            .collect();
        match options.mode {
            PptxMode::Image => {
                let mut renderer = PageRenderer::new(self.display.clone(), self.config.clone());
                for (slide, &p) in slides.iter_mut().zip(&pages) {
                    match render_png(&mut renderer, p, dpi, false) {
                        Ok(png) => slide.background = png,
                        Err(e) => warnings.push(e),
                    }
                }
            }
            PptxMode::Editable => self.editable_slides(&pages, &mut slides, dpi, &mut warnings),
        }
        Ok((slides, warnings))
    }

    fn editable_slides(&self, pages: &[usize], slides: &mut [Slide], dpi: f64, warnings: &mut Vec<String>) {
        let Some(cos) = self.editor.as_ref().map(|e| &e.cos) else {
            // Nothing to take apart: each page is its own background.
            let mut renderer = PageRenderer::new(self.display.clone(), self.config.clone());
            for (slide, &p) in slides.iter_mut().zip(pages) {
                match render_png(&mut renderer, p, dpi, true) {
                    Ok(png) => slide.background = png,
                    Err(e) => warnings.push(e),
                }
            }
            return;
        };
        let mut placed: HashSet<pdfcraft_cos::ObjRef> = HashSet::new();
        for (slide, &p) in slides.iter_mut().zip(pages) {
            let Some(info) = self.info.pages.get(p) else { continue };
            let view = View::of(info);
            for (b, frame) in pdfcraft_edit::reading_blocks_framed(cos, p).unwrap_or_default() {
                if b.text.trim().is_empty() {
                    continue;
                }
                let f = b.base_font.to_ascii_lowercase();
                slide.texts.push(SlideText {
                    font: pdfcraft_export::font_family(&b.base_font),
                    size: b.size * view.scale(),
                    bold: b.bold || f.contains("bold") || f.contains("black") || f.contains("heavy"),
                    italic: b.italic || f.contains("italic") || f.contains("oblique"),
                    color: b.color,
                    lines: b.line_count,
                    at: view.place(frame.centre, frame.width, frame.height, frame.angle, false),
                    text: b.text,
                });
            }
            for im in pdfcraft_edit::reading_images(cos, p).unwrap_or_default() {
                let Some(obj) = im.object else { continue };
                let Ok((_, bytes)) = pdfcraft_create::image_file(cos, obj) else { continue };
                let [a, b, c, d, e, f] = im.matrix;
                let centre = [0.5 * a + 0.5 * c + e, 0.5 * b + 0.5 * d + f];
                let angle = b.atan2(a).to_degrees();
                let flip = a * d - b * c < 0.0;
                slide
                    .pictures
                    .push(SlidePicture { bytes, at: view.place(centre, a.hypot(b), c.hypot(d), if angle.is_finite() { angle } else { 0.0 }, flip) });
                placed.insert(obj);
            }
        }
        // The background: a copy of the document without what the slides now hold.
        let mut scratch = cos.clone();
        let mut done = HashSet::new();
        let mut stripped = vec![true; slides.len()];
        for (k, &p) in pages.iter().enumerate() {
            if let Err(e) = pdfcraft_edit::strip_text_and_images(&mut scratch, p, &placed, &mut done) {
                warnings.push(format!("page {}: no background ({e})", p.saturating_add(1)));
                if let Some(s) = stripped.get_mut(k) {
                    *s = false;
                }
            }
        }
        let bytes = match background_bytes(&scratch) {
            Ok(b) => Arc::new(b),
            Err(e) => {
                warnings.push(format!("no page backgrounds: {e}"));
                return;
            }
        };
        let mut renderer = PageRenderer::new(bytes, self.config.clone());
        for ((slide, &p), ok) in slides.iter_mut().zip(pages).zip(stripped) {
            if !ok {
                continue;
            }
            match render_png(&mut renderer, p, dpi, true) {
                Ok(png) => slide.background = png,
                Err(e) => warnings.push(e),
            }
        }
    }
}

/// The scratch copy as the renderer reads it, with the appearances the page view draws for
/// comments and fields that have none.
fn background_bytes(cos: &pdfcraft_cos::Document) -> Result<Vec<u8>, String> {
    let comments = pdfcraft_annot::with_missing_appearances(cos);
    let widgets = pdfcraft_forms::with_missing_appearances(comments.as_ref().unwrap_or(cos));
    let shown = widgets.as_ref().or(comments.as_ref()).unwrap_or(cos);
    let written = if crate::certificate_secured(shown) {
        pdfcraft_cos::write_decrypted_view(shown)
    } else {
        pdfcraft_cos::write_incremental(shown, &pdfcraft_cos::SaveOptions::default())
    };
    written.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DocId, Session};

    /// A PDF from object bodies (object i + 1 is `objs[i]`; 1 is the catalog, 2 the page tree).
    fn pdf(objs: &[Vec<u8>]) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend(o);
            out.extend(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
        for o in offsets {
            out.extend(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
        out
    }

    fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
        let mut v = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
        v.extend(data);
        v.extend(b"\nendstream");
        v
    }

    /// Page 1: bold red text over a blue rectangle; 2: an image; 3: /Rotate 90 with upright and
    /// turned text; 4: empty.
    fn sample() -> (Session, DocId) {
        let res = "/Resources << /Font << /F1 5 0 R >> /XObject << /Im1 6 0 R >> >>";
        let objs = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R 4 0 R 8 0 R 10 0 R] /Count 4 >>".to_vec(),
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] {res} /Contents 7 0 R >>").into_bytes(),
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] {res} /Contents 9 0 R >>").into_bytes(),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>".to_vec(),
            stream(
                "/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8",
                &[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0],
            ),
            stream("", b"0 0 1 rg 100 100 200 50 re f 1 0 0 rg BT /F1 24 Tf 72 700 Td (Hello slide) Tj ET"),
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Rotate 90 {res} /Contents 11 0 R >>").into_bytes(),
            stream("", b"q 200 0 0 100 50 50 cm /Im1 Do Q"),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_vec(),
            stream("", b"BT /F1 20 Tf 72 500 Td (Sideways) Tj ET BT /F1 20 Tf 0 1 -1 0 300 100 Tm (Upright) Tj ET"),
        ];
        let mut s = Session::new();
        let id = s.open("deck.pdf", None, Arc::new(pdf(&objs)), None).unwrap();
        (s, id)
    }

    fn near(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn pages_become_text_boxes_pictures_and_backgrounds() {
        let (s, id) = sample();
        let d = s.get(id).unwrap();
        let (slides, warnings) = d.pptx_slides(&PptxOptions::default()).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(slides.len(), 4);

        // Page 1: the text box where the text is, styled; the rectangle in the background.
        let t = &slides[0].texts;
        assert_eq!(t.len(), 1, "{t:?}");
        assert_eq!((t[0].text.as_str(), t[0].font.as_str(), t[0].bold, t[0].italic), ("Hello slide", "Arial", true, false));
        assert_eq!(t[0].color, [1.0, 0.0, 0.0]);
        assert!(near(t[0].size, 24.0, 0.01));
        assert!(near(t[0].at.x, 72.0, 1.0) && t[0].at.y > 792.0 - 700.0 - 30.0 && t[0].at.y < 792.0 - 700.0, "{:?}", t[0].at);
        assert_eq!(t[0].at.rot, 0.0);
        assert!(slides[0].background.is_some(), "the blue rectangle needs a background");

        // Page 2: the image as a picture, top-left at (50, 792 - 150); nothing left behind it.
        let p = &slides[1].pictures;
        assert_eq!(p.len(), 1);
        assert!(p[0].bytes.starts_with(b"\x89PNG"));
        let at = p[0].at;
        assert!(near(at.x, 50.0, 0.01) && near(at.y, 642.0, 0.01) && near(at.w, 200.0, 0.01) && near(at.h, 100.0, 0.01), "{at:?}");
        assert!(!at.flip_v && at.rot == 0.0);
        assert!(slides[1].background.is_none(), "the image was the only thing on the page");

        // Page 3 displays 792 × 612: upright text turns with the page, text set at 90° reads upright.
        assert_eq!((slides[2].width, slides[2].height), (792.0, 612.0));
        let side = slides[2].texts.iter().find(|t| t.text == "Sideways").unwrap();
        assert!(near(side.at.rot, 90.0, 0.01), "{:?}", side.at);
        // Its centre is (centre y, centre x) of the user-space box.
        let (cx, cy) = (side.at.x + side.at.w / 2.0, side.at.y + side.at.h / 2.0);
        assert!(near(cx, 507.0, 8.0) && cy > 72.0 && cy < 200.0, "{:?}", side.at);
        let up = slides[2].texts.iter().find(|t| t.text == "Upright").unwrap();
        assert!(near(up.at.rot.rem_euclid(360.0), 0.0, 0.01), "{:?}", up.at);
        assert!(slides[2].background.is_none());

        // Page 4 is empty.
        assert!(slides[3].texts.is_empty() && slides[3].pictures.is_empty() && slides[3].background.is_none());

        let out = d.export_pptx(&PptxOptions::default()).unwrap();
        assert_eq!(out.stats, PptxStats { slides: 4, text_boxes: 3, pictures: 1, backgrounds: 1, omitted: 0 });
        assert!(out.bytes.starts_with(b"PK\x03\x04"));
        if let Ok(dir) = std::env::var("PDFCRAFT_EXPORT_DIR") {
            std::fs::write(format!("{dir}/deck.pptx"), &out.bytes).unwrap();
        }
    }

    #[test]
    fn image_mode_and_page_choice() {
        let (s, id) = sample();
        let d = s.get(id).unwrap();
        let out = d.export_pptx(&PptxOptions { pages: vec![3, 0], mode: PptxMode::Image, dpi: 36.0 }).unwrap();
        assert_eq!(out.stats, PptxStats { slides: 2, text_boxes: 0, pictures: 0, backgrounds: 2, omitted: 0 });
        let (slides, _) = d.pptx_slides(&PptxOptions { pages: vec![2], mode: PptxMode::Image, dpi: 72.0 }).unwrap();
        assert_eq!((slides[0].width, slides[0].height), (792.0, 612.0));
        assert!(d.export_pptx(&PptxOptions { pages: vec![4], ..Default::default() }).unwrap_err().contains("page 5 does not exist"));
        assert!(d.export_pptx(&PptxOptions { pages: vec![usize::MAX], ..Default::default() }).is_err());
        // Out-of-range resolutions are clamped, not trusted.
        for dpi in [f64::NAN, -1.0, 1e12] {
            assert_eq!(d.export_pptx(&PptxOptions { pages: vec![0], mode: PptxMode::Image, dpi }).unwrap().stats.slides, 1);
        }
        assert_eq!(PptxMode::from_id("image"), Some(PptxMode::Image));
        assert_eq!(PptxMode::from_id("Editable"), None);
    }

    #[test]
    fn hostile_pages_export_without_panicking() {
        // A cyclic form, an unreadable image, a broken content stream, NaN matrices, a zero box.
        let objs = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R 7 0 R] /Count 2 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Rotate 45 /Resources << /XObject << /Fm 4 0 R /Im 5 0 R >> /Font << /F1 6 0 R >> >> /Contents 8 0 R >>".to_vec(),
            stream("/Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources << /XObject << /Fm 4 0 R >> /Font << /F1 6 0 R >> >>", b"/Fm Do BT /F1 9 Tf (loop) Tj ET"),
            stream("/Type /XObject /Subtype /Image /Width 99999 /Height 99999 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode", b"garbage"),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /ABCDEF+ >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 0 0] /Contents 9 0 R >>".to_vec(),
            stream("", b"q 1e400 0 0 nan 0 0 cm /Im Do Q /Fm Do BT /F1 1e30 Tf (big) Tj ET ( unbalanced"),
            stream("/Filter /FlateDecode", b"not deflate"),
        ];
        let mut s = Session::new();
        let Ok(id) = s.open("bad.pdf", None, Arc::new(pdf(&objs)), None) else { return };
        let d = s.get(id).unwrap();
        for mode in [PptxMode::Editable, PptxMode::Image] {
            let out = d.export_pptx(&PptxOptions { mode, ..Default::default() }).unwrap();
            assert_eq!(out.stats.slides, d.info.pages.len());
        }
    }
}
