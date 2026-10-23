//! Insert pages from the clipboard and create a PDF from a screen capture, headlessly.
//!
//! There is no system clipboard or screen here, so the caller passes what the clipboard holds
//! (text, an image file, or raw RGBA pixels) or the file a capture tool wrote; the conversion is
//! the app's (`pdfcraft_engine::clip`).

use base64::Engine as _;
use pdfcraft_engine::clip::{MAX_CAPTURE_BYTES, MAX_CLIP_PIXELS};
use pdfcraft_engine::{CaptureRegion, Clip, Edit, SourceKind};
use serde_json::Value;
use std::sync::Arc;

use crate::{Args, Automation, Result, ToolError, failed, summary};

fn bad(m: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs(m.into())
}

/// Read `path` (resolved under `--root`) if it is at most `max` bytes.
fn read_capped(app: &Automation, path: &str, max: usize) -> Result<(String, Vec<u8>)> {
    let p = app.resolve(path, false)?;
    let len = std::fs::metadata(&p).map_err(|e| failed(format!("{path}: {e}")))?.len();
    if len > max as u64 {
        return Err(failed(format!("{path} is {len} bytes; at most {} MiB", max >> 20)));
    }
    let bytes = std::fs::read(&p).map_err(|e| failed(format!("{path}: {e}")))?;
    Ok((p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), bytes))
}

fn dimension(a: &Args, key: &str) -> Result<usize> {
    let v = a.int(key)?;
    usize::try_from(v).ok().filter(|&n| n >= 1).ok_or_else(|| bad(format!("{key} must be a positive number of pixels")))
}

impl Automation {
    pub(crate) fn page_insert_clipboard(&mut self, a: &Args) -> Result<Value> {
        let n = self.doc(a)?.info.pages.len();
        let before = match a.opt_str("position")? {
            None | Some("after") => false,
            Some("before") => true,
            Some(other) => return Err(bad(format!("position must be \"before\" or \"after\", not {other:?}"))),
        };
        let page = match a.opt_int("page")? {
            None => n.saturating_sub(1),
            Some(_) if n == 0 => 0,
            Some(_) => self.page(a)?,
        };
        let at = if before || n == 0 { page } else { page.saturating_add(1) };
        let sources = ["text", "image_path", "rgba_base64"].iter().filter(|k| a.get(k).is_some()).count();
        if sources != 1 {
            return Err(bad("pass exactly one of text, image_path or rgba_base64 (with width and height)"));
        }
        let (edit, pages) = if let Some(t) = a.opt_str("text")? {
            self.session.insert_clip_edit(&Clip::Text(t.to_string()), at).map_err(failed)?
        } else if let Some(b) = a.opt_str("rgba_base64")? {
            let (width, height) = (dimension(a, "width")?, dimension(a, "height")?);
            let expected = width
                .checked_mul(height)
                .filter(|&p| p <= MAX_CLIP_PIXELS)
                .ok_or_else(|| bad(format!("the image is too large; at most {MAX_CLIP_PIXELS} pixels")))?;
            if b.len() > expected.saturating_mul(4).saturating_div(3).saturating_mul(4).saturating_add(8) {
                return Err(bad("rgba_base64 is longer than width × height × 4 bytes"));
            }
            let clean: String = b.chars().filter(|c| !c.is_ascii_whitespace()).collect();
            let rgba = base64::engine::general_purpose::STANDARD
                .decode(clean.as_bytes())
                .map_err(|e| bad(format!("rgba_base64 is not valid base64: {e}")))?;
            self.session.insert_clip_edit(&Clip::Image { width, height, rgba }, at).map_err(failed)?
        } else {
            let (name, bytes) = read_capped(self, a.str("image_path")?, MAX_CAPTURE_BYTES)?;
            if pdfcraft_engine::source_kind(&name, &bytes) != Some(SourceKind::Image) {
                return Err(bad(format!("{name} is not an image (PNG, JPEG, TIFF, GIF or BMP)")));
            }
            let (_, pdf) = self.session.convert_to_pdf(&name, &Arc::new(bytes)).map_err(failed)?;
            (Edit::InsertPagesFrom { name: "Clipboard".into(), bytes: pdf, pages: None, at, merge_tags: true }, 1)
        };
        let mut out = self.apply(a, edit)?;
        out["inserted"] = serde_json::json!({ "first": at.saturating_add(1), "count": pages });
        Ok(out)
    }

    pub(crate) fn doc_create_screenshot(&mut self, a: &Args) -> Result<Value> {
        let (_, bytes) = read_capped(self, a.str("path")?, MAX_CAPTURE_BYTES)?;
        let region = match a.get("region") {
            None => None,
            Some(r) => {
                let num = |k: &str| {
                    r.get(k)
                        .and_then(Value::as_u64)
                        .and_then(|v| u32::try_from(v).ok())
                        .ok_or_else(|| bad(format!("region.{k} must be a whole number of pixels")))
                };
                Some(CaptureRegion { x: num("x")?, y: num("y")?, width: num("width")?, height: num("height")? })
            }
        };
        let pdf = self.session.pdf_from_capture(&bytes, region).map_err(failed)?;
        let name = a.opt_str("name")?.map_or_else(|| "Screen capture.pdf".to_string(), str::to_owned);
        let id = self.session.open_new(name, pdf).map_err(failed)?;
        Ok(summary(self.session.get(id).ok_or_else(|| failed("the document vanished"))?))
    }
}
