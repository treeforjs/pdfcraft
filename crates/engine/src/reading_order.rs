//! The Reading Order tool and the Order panel on an open document: [`a11y::page_order`] and
//! [`a11y::tag_region`] with the renderer's text extraction as their eyes (see
//! [`crate::autotag::with_glyphs`]). The edits themselves are [`crate::Edit::TagRegion`],
//! [`crate::Edit::MoveTag`] and [`crate::Edit::ClearPageStructure`], one undo step each.

use crate::autotag::with_glyphs;
use crate::{Document, a11y, guard};

impl Document {
    /// Reading Order tool / Order panel: page `page`'s (0-based) regions in reading order, with
    /// where each is drawn, and how much of the page is untagged or artifact.
    pub fn reading_order(&self, page: usize) -> Result<a11y::PageOrder, String> {
        let editor = self.editor.as_ref().ok_or_else(|| self.read_only_reason.clone().unwrap_or_else(|| "the document can't be read".into()))?;
        let cos = &editor.cos;
        guard(|| with_glyphs(|text| a11y::page_order(cos, page, text).map_err(|e| e.to_string())))
            .map_err(|m| format!("reading the reading order failed unexpectedly ({m})"))?
    }

    /// Reading Order tool ▸ tag a region, tried on a copy: what [`crate::Edit::TagRegion`] would
    /// do, or why it is refused. The document itself is left as it is.
    pub fn tag_region_preview(&self, request: &a11y::RegionTag) -> Result<a11y::RegionResult, String> {
        let editor = self.editor.as_ref().ok_or_else(|| self.read_only_reason.clone().unwrap_or_else(|| "the document can't be edited".into()))?;
        let mut copy = editor.cos.clone();
        guard(|| run_tag(&mut copy, request)).map_err(|m| format!("tagging the region failed unexpectedly ({m}); the document was not changed"))?
    }

    /// Clear Page Structure, tried on a copy: what [`crate::Edit::ClearPageStructure`] would
    /// remove, or why it is refused.
    pub fn clear_page_structure_preview(&self, page: usize, keep_artifacts: bool) -> Result<a11y::ClearResult, String> {
        let editor = self.editor.as_ref().ok_or_else(|| self.read_only_reason.clone().unwrap_or_else(|| "the document can't be edited".into()))?;
        let mut copy = editor.cos.clone();
        guard(|| a11y::clear_page_structure(&mut copy, page, keep_artifacts).map_err(|e| e.to_string()))
            .map_err(|m| format!("clearing the page structure failed unexpectedly ({m}); the document was not changed"))?
    }
}

/// Tag a region of `doc` in place (all or nothing).
pub(crate) fn run_tag(doc: &mut pdfcraft_cos::Document, request: &a11y::RegionTag) -> Result<a11y::RegionResult, String> {
    with_glyphs(|text| a11y::tag_region(doc, request, text).map_err(|e| e.to_string()))
}
