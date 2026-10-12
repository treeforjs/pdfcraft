//! Render a proposed paragraph edit using the same PDF content that an accepted edit writes.

use std::sync::Arc;

use crate::{BlockStyle, Document, Edit, check_permission, guard};

pub struct TextEditPreview {
    bytes: Arc<Vec<u8>>,
    config: pdfcraft_render::RenderConfig,
    request: pdfcraft_render::RenderRequest,
}

impl TextEditPreview {
    /// Queue the bounded preview on a worker for a responsive desktop dialog.
    pub fn renderer(&self) -> pdfcraft_render::RenderPool {
        let renderer = pdfcraft_render::RenderPool::new(self.bytes.clone(), 1, self.config.clone());
        renderer.set_queue(vec![self.request]);
        renderer
    }

    /// Synchronous CLI/MCP counterpart, using the same PDF renderer and pixel limits.
    pub fn render(&self) -> Result<pdfcraft_render::RenderedPage, String> {
        guard(|| {
            let mut renderer = pdfcraft_render::PageRenderer::new(self.bytes.clone(), self.config.clone());
            let output = renderer.render(self.request);
            if let Some(error) = &output.error {
                return Err(error.clone());
            }
            Ok(output)
        })?
    }
}

impl Document {
    /// A proposed paragraph edit, rendered without changing working bytes, dirty state,
    /// selections or undo/redo. Embedded PDF fonts and exact content geometry are preserved.
    pub fn text_edit_preview(&self, page: usize, block: usize, text: &str, style: &BlockStyle, max_side: u32) -> Result<TextEditPreview, String> {
        pdfcraft_edit::text::validate_paragraph_text(text).map_err(|error| error.to_string())?;
        if !(128..=4096).contains(&max_side) {
            return Err("preview max_side must be from 128 to 4096 pixels".into());
        }
        guard(|| {
            let editor = self.editor.as_ref().ok_or("the document cannot be edited")?;
            if let Some(permissions) = editor.cos.permissions() {
                check_permission(&Edit::EditTextBlock { page, block, text: text.into(), style: style.clone() }, &permissions)
                    .map_err(|e| e.to_string())?;
            }
            let info = self.info.pages.get(page).ok_or("the page does not exist")?;
            let mut cos = editor.cos.clone();
            pdfcraft_edit::rewrite_block(&mut cos, page, block, Some(text), style).map_err(|e| e.to_string())?;
            let bytes = pdfcraft_cos::write_incremental(&cos, &pdfcraft_cos::SaveOptions::default()).map_err(|e| e.to_string())?;
            Ok(TextEditPreview {
                bytes: Arc::new(bytes),
                config: self.config.clone(),
                request: pdfcraft_render::RenderRequest { page, scale: max_side as f32 / info.width.max(info.height).max(1.0), ..Default::default() },
            })
        })?
    }
}
