//! Autotag Document: [`a11y::autotag`] with the renderer's text extraction as its eyes. The
//! provisional copy the autotagger builds (every painting operation in its own marked content)
//! is written out unencrypted in memory, its pages' text is extracted, and each glyph comes
//! back with the operation it belongs to.

use std::sync::Arc;

use pdfcraft_render::{PageRenderer, RenderConfig, RenderRequest, RequestKind};

use crate::{Document, a11y, guard};

/// The glyphs of `pages` (0-based) of a provisional document, each with the marked-content id
/// around it, read by the renderer; `None` for a page whose text can't be read.
pub(crate) fn read_glyphs(provisional: &pdfcraft_cos::Document, pages: &[usize]) -> Result<Vec<Option<Vec<a11y::AutotagGlyph>>>, String> {
    let bytes =
        pdfcraft_cos::write_decrypted_view(provisional).map_err(|e| format!("the pages could not be prepared for reading their text: {e}"))?;
    let mut renderer = PageRenderer::new(Arc::new(bytes), RenderConfig::default());
    Ok(pages
        .iter()
        .map(|p| {
            let text = renderer.render(RenderRequest { page: *p, kind: RequestKind::Text, scale: 1.0, ..Default::default() }).text?;
            Some(
                text.glyphs
                    .iter()
                    .filter_map(|g| {
                        let mcid = i64::from(g.mcid?);
                        Some(a11y::AutotagGlyph { text: g.text.clone(), rect: g.rect.map(f64::from), mcid })
                    })
                    .collect(),
            )
        })
        .collect())
}

/// Run `f` with the renderer as its text extractor; a failure to prepare the pages wins over
/// `f`'s own result.
pub(crate) fn with_glyphs<T>(f: impl FnOnce(&mut a11y::AutotagText<'_>) -> Result<T, String>) -> Result<T, String> {
    let mut failure: Option<String> = None;
    let mut text = |provisional: &pdfcraft_cos::Document, pages: &[usize]| -> Vec<Option<Vec<a11y::AutotagGlyph>>> {
        match read_glyphs(provisional, pages) {
            Ok(g) => g,
            Err(e) => {
                failure = Some(e);
                Vec::new()
            }
        }
    };
    let result = f(&mut text);
    match failure {
        Some(f) => Err(f),
        None => result,
    }
}

/// Tag `doc` in place (it is the caller's copy: on an error it may be half changed).
pub(crate) fn run(doc: &mut pdfcraft_cos::Document, options: &a11y::AutotagOptions) -> Result<a11y::AutotagReport, String> {
    with_glyphs(|text| a11y::autotag(doc, options, text).map_err(|e| e.to_string()))
}

impl Document {
    /// Autotag Document, tried on a copy: what [`crate::Edit::Autotag`] would do (elements by
    /// type, artifacts, figures that need alternate text, links), or why it is refused. The
    /// document itself is left as it is.
    pub fn autotag_preview(&self, options: &a11y::AutotagOptions) -> Result<a11y::AutotagReport, String> {
        let editor = self.editor.as_ref().ok_or_else(|| self.read_only_reason.clone().unwrap_or_else(|| "the document can't be edited".into()))?;
        let mut copy = editor.cos.clone();
        guard(|| run(&mut copy, options)).map_err(|m| format!("autotagging failed unexpectedly ({m}); the document was not changed"))?
    }
}
