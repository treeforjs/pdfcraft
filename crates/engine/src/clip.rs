//! Clipboard contents and screen captures as PDF pages (Create ▸ Clipboard, Organize ▸ Insert
//! from clipboard, Create ▸ Screen capture).
//!
//! The frontends read the system clipboard or run the operating system's capture tool; this
//! module turns what they got into PDF bytes, so the app and the automation tools share one
//! conversion. Everything here is untrusted input: sizes are checked before anything is
//! allocated from them, and image decoding runs under [`crate::guard`].

use std::sync::Arc;

use crate::{EditError, Session};
use pdfcraft_create::CreateError;

/// The largest clipboard or capture image accepted, in pixels (16 384 × 16 384 would be 1 GiB
/// of RGBA; this is 8K × 8K, more than any screen).
pub const MAX_CLIP_PIXELS: usize = 8192 * 8192;
/// The longest clipboard text accepted, in bytes.
pub const MAX_CLIP_TEXT: usize = 8 << 20;
/// The largest captured image file accepted, in bytes.
pub const MAX_CAPTURE_BYTES: usize = 256 << 20;

/// What was on the clipboard.
#[derive(Clone, Debug, PartialEq)]
pub enum Clip {
    /// RGBA pixels, row by row.
    Image {
        width: usize,
        height: usize,
        rgba: Vec<u8>,
    },
    Text(String),
}

/// Part of a captured image, in its pixels (origin top left).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

fn invalid(msg: impl Into<String>) -> EditError {
    EditError::Create(CreateError::Invalid(msg.into()))
}

/// A PNG of `rgba` (`width` × `height` pixels, four bytes each).
pub fn png_from_rgba(width: usize, height: usize, rgba: &[u8]) -> Result<Vec<u8>, EditError> {
    let pixels = width.checked_mul(height).filter(|&p| p > 0).ok_or_else(|| invalid("the image is empty"))?;
    if pixels > MAX_CLIP_PIXELS {
        return Err(invalid(format!("the image is too large ({width} × {height} pixels; at most {MAX_CLIP_PIXELS} pixels)")));
    }
    if pixels.checked_mul(4) != Some(rgba.len()) {
        return Err(invalid("the image is malformed: its pixel data doesn't match its size"));
    }
    let (w, h) =
        (u32::try_from(width).map_err(|_| invalid("the image is too wide"))?, u32::try_from(height).map_err(|_| invalid("the image is too tall"))?);
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(|e| invalid(e.to_string()))?;
    writer.write_image_data(rgba).map_err(|e| invalid(e.to_string()))?;
    writer.finish().map_err(|e| invalid(e.to_string()))?;
    Ok(out)
}

impl Session {
    /// A new PDF from clipboard contents: an image becomes one page at its size, text flows
    /// onto US Letter pages. Returns the bytes and their page count.
    pub fn pdf_from_clip(&self, clip: &Clip) -> Result<(Arc<Vec<u8>>, usize), EditError> {
        match clip {
            Clip::Image { width, height, rgba } => {
                let png = png_from_rgba(*width, *height, rgba)?;
                Ok((self.create_from_images(&[("Clipboard.png".into(), png)])?, 1))
            }
            Clip::Text(t) => {
                if t.trim().is_empty() {
                    return Err(invalid("the clipboard text is empty"));
                }
                if t.len() > MAX_CLIP_TEXT {
                    return Err(invalid(format!("the clipboard text is too long ({} bytes; at most {MAX_CLIP_TEXT})", t.len())));
                }
                let bytes = self.create_from_text("Clipboard", t)?;
                let pages = self.page_count_of("Clipboard.pdf", &bytes)?;
                Ok((bytes, pages))
            }
        }
    }

    /// The edit that inserts clipboard contents so they start at page index `at` (0 = before
    /// the first page; clamped to the end), as one undo step. Returns it and the page count.
    pub fn insert_clip_edit(&self, clip: &Clip, at: usize) -> Result<(crate::Edit, usize), EditError> {
        let (bytes, pages) = self.pdf_from_clip(clip)?;
        Ok((crate::Edit::InsertPagesFrom { name: "Clipboard".into(), bytes, pages: None, at, merge_tags: true }, pages))
    }

    /// A new one-page PDF from a screen capture (PNG, JPEG or TIFF bytes from the operating
    /// system's capture tool), optionally cut down to `region` of it.
    pub fn pdf_from_capture(&self, image: &[u8], region: Option<CaptureRegion>) -> Result<Arc<Vec<u8>>, EditError> {
        if image.is_empty() {
            return Err(invalid("the screen capture is empty"));
        }
        if image.len() > MAX_CAPTURE_BYTES {
            return Err(invalid(format!("the screen capture is too large ({} bytes; at most {MAX_CAPTURE_BYTES})", image.len())));
        }
        let name = "Screen capture.png".to_string();
        let Some(r) = region else {
            return crate::guard(|| self.create_from_images(&[(name, image.to_vec())]))
                .map_err(|_| invalid("the screen capture could not be read"))?;
        };
        let png = crate::guard(|| crop_capture(image, r)).map_err(|_| invalid("the screen capture could not be read"))??;
        self.create_from_images(&[(name, png)])
    }
}

/// Decode `image` (within [`MAX_CLIP_PIXELS`]) and return `region` of it as a PNG.
fn crop_capture(image: &[u8], r: CaptureRegion) -> Result<Vec<u8>, EditError> {
    if r.width == 0 || r.height == 0 {
        return Err(invalid("the capture region is empty"));
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(image)).with_guessed_format().map_err(|e| invalid(e.to_string()))?;
    reader.limits(decode_limits());
    let (w, h) = reader.into_dimensions().map_err(|e| invalid(format!("the screen capture could not be read: {e}")))?;
    let fits = |start: u32, len: u32, size: u32| start.checked_add(len).is_some_and(|end| end <= size);
    if !fits(r.x, r.width, w) || !fits(r.y, r.height, h) {
        return Err(invalid(format!("the capture region ({}, {}, {} × {}) is outside the {w} × {h} pixel capture", r.x, r.y, r.width, r.height)));
    }
    // `into_dimensions` consumed the reader; read again to decode.
    let mut reader = image::ImageReader::new(std::io::Cursor::new(image)).with_guessed_format().map_err(|e| invalid(e.to_string()))?;
    reader.limits(decode_limits());
    let decoded = reader.decode().map_err(|e| invalid(format!("the screen capture could not be read: {e}")))?;
    let cropped = decoded.crop_imm(r.x, r.y, r.width, r.height).to_rgba8();
    png_from_rgba(r.width as usize, r.height as usize, cropped.as_raw())
}

/// Decoding stops before allocating more than an 8K × 8K RGBA image (and a copy).
fn decode_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_alloc = Some((MAX_CLIP_PIXELS as u64).saturating_mul(8));
    limits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x * 7) as u8, (y * 5) as u8, 90, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn clipboard_images_and_text_become_pages() {
        let s = Session::new();
        let (pdf, n) = s.pdf_from_clip(&Clip::Image { width: 3, height: 2, rgba: vec![200; 24] }).unwrap();
        assert_eq!(n, 1);
        assert_eq!(s.page_count_of("x.pdf", &pdf).unwrap(), 1);
        let long = "line\n".repeat(200);
        let (pdf, n) = s.pdf_from_clip(&Clip::Text(long)).unwrap();
        assert!(n > 1);
        assert_eq!(s.page_count_of("x.pdf", &pdf).unwrap(), n);
    }

    #[test]
    fn hostile_clipboard_contents_are_refused() {
        let s = Session::new();
        for clip in [
            Clip::Image { width: 0, height: 5, rgba: vec![] },
            Clip::Image { width: 2, height: 2, rgba: vec![0; 15] },
            Clip::Image { width: usize::MAX, height: 2, rgba: vec![0; 16] },
            Clip::Image { width: 1 << 20, height: 1 << 20, rgba: vec![0; 16] },
            Clip::Text("   \n\t".into()),
            Clip::Text("x".repeat(MAX_CLIP_TEXT + 1)),
        ] {
            assert!(s.pdf_from_clip(&clip).is_err(), "{:?}", std::mem::discriminant(&clip));
        }
    }

    #[test]
    fn captures_become_one_page_and_crop_to_a_region() {
        let mut s = Session::new();
        let shot = png(40, 30);
        let whole = s.pdf_from_capture(&shot, None).unwrap();
        let part = s.pdf_from_capture(&shot, Some(CaptureRegion { x: 10, y: 5, width: 20, height: 10 })).unwrap();
        for (bytes, size) in [(whole, (40.0, 30.0)), (part, (20.0, 10.0))] {
            let id = s.open_new("Screen capture.pdf", bytes).unwrap();
            let pages = &s.get(id).unwrap().info.pages;
            assert_eq!(pages.len(), 1);
            assert_eq!((pages[0].width, pages[0].height), size);
        }
    }

    #[test]
    fn hostile_captures_are_refused() {
        let s = Session::new();
        let shot = png(40, 30);
        assert!(s.pdf_from_capture(&[], None).is_err());
        assert!(s.pdf_from_capture(b"not an image at all", None).is_err());
        assert!(s.pdf_from_capture(&shot[..shot.len() / 2], None).is_err());
        for r in [
            CaptureRegion { x: 0, y: 0, width: 0, height: 10 },
            CaptureRegion { x: 30, y: 0, width: 11, height: 10 },
            CaptureRegion { x: u32::MAX, y: 0, width: 2, height: 2 },
            CaptureRegion { x: 0, y: 25, width: 5, height: u32::MAX },
        ] {
            assert!(s.pdf_from_capture(&shot, Some(r)).is_err(), "{r:?}");
        }
        assert!(s.pdf_from_capture(b"garbage", Some(CaptureRegion { x: 0, y: 0, width: 1, height: 1 })).is_err());
    }
}
