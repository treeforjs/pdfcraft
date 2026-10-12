//! Reader selections use whole extracted glyphs, including indivisible ligature mappings.
//! Page and glyph indices are zero-based here; automation translates its one-based inputs.

use crate::Document;
use pdfcraft_render::{PageRenderer, PageText, RenderConfig, RenderRequest, RequestKind};
use std::ops::Range;

/// Maximum UTF-8 text returned by a single clipboard/extraction request.
pub const MAX_SELECTION_BYTES: usize = 32 * 1024 * 1024;

/// Why text extraction was refused (ISO 32000-2 Table 22 bit 5 is clear, or an encrypted
/// document's permissions could not be established).
pub const COPY_DENIED: &str = "this document does not allow copying text; open it with the permissions password";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextPosition {
    pub page: usize,
    pub glyph: usize,
}

/// Inclusive endpoints retain the direction/anchor of a pointer selection. Output is always
/// in document reading order. `All` includes pages whose text has not been loaded yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextSelection {
    Range { anchor: TextPosition, head: TextPosition },
    All,
}

impl TextSelection {
    pub fn ordered(self) -> Option<(TextPosition, TextPosition)> {
        match self {
            Self::Range { anchor, head } => Some((anchor.min(head), anchor.max(head))),
            Self::All => None,
        }
    }

    pub fn pages(self, count: usize) -> Range<usize> {
        match self.ordered() {
            Some((first, last)) => first.page.min(count)..last.page.saturating_add(1).min(count),
            None => 0..count,
        }
    }

    pub fn range_on(self, page: usize, glyphs: usize) -> Option<Range<usize>> {
        match self.ordered() {
            Some((first, last)) if page < first.page || page > last.page => None,
            Some((first, last)) => Some(
                (if page == first.page { first.glyph.min(glyphs) } else { 0 })..(if page == last.page {
                    last.glyph.saturating_add(1).min(glyphs)
                } else {
                    glyphs
                }),
            ),
            None => Some(0..glyphs),
        }
    }

    pub fn extend(&mut self, at: TextPosition) -> bool {
        if let Self::Range { head, .. } = self {
            let changed = *head != at;
            *head = at;
            changed
        } else {
            false
        }
    }

    pub fn validate_page_count(self, count: usize) -> Result<(), String> {
        if self.ordered().is_some_and(|(_, last)| last.page >= count) {
            return Err(format!("selection page is out of range: the document has {count} pages"));
        }
        Ok(())
    }

    fn validate_endpoint(self, page: usize, text: &PageText) -> Result<(), String> {
        if let Some((first, last)) = self.ordered() {
            for endpoint in [first, last] {
                if endpoint.page == page && endpoint.glyph >= text.glyphs.len() {
                    return Err(format!("selection glyph is out of range on page {}: it has {} glyphs", page.saturating_add(1), text.glyphs.len()));
                }
            }
        }
        Ok(())
    }
}

fn append(out: &mut String, text: &str) -> Result<(), String> {
    if text.len() > MAX_SELECTION_BYTES.saturating_sub(out.len()) {
        return Err("selected text exceeds the 32 MiB copy limit; select a smaller range".into());
    }
    out.push_str(text);
    Ok(())
}

/// Append one page without allocating an unbounded intermediate string. Blank pages still
/// contribute their page boundary; line breaks, word gaps and multi-scalar glyphs are retained.
pub fn append_selection_page(out: &mut String, selection: TextSelection, page: usize, text: &PageText, separator: bool) -> Result<(), String> {
    selection.validate_endpoint(page, text)?;
    let Some(range) = selection.range_on(page, text.glyphs.len()) else { return Ok(()) };
    if separator {
        append(out, "\n\n")?;
    }
    let mut previous = None;
    for (i, glyph) in text.glyphs.iter().enumerate().skip(range.start).take(range.len()) {
        let line = text.line_of.get(i).copied();
        if i != range.start {
            if line != previous {
                append(out, "\n")?;
            } else if text.space_before.get(i).copied().unwrap_or(false) {
                append(out, " ")?;
            }
        }
        append(out, &glyph.text)?;
        previous = line;
    }
    Ok(())
}

/// Copy from an already loaded text cache. `None` means incomplete, never a partial copy.
pub fn cached_selection_text<'a>(
    selection: TextSelection,
    count: usize,
    mut page_text: impl FnMut(usize) -> Option<&'a PageText>,
) -> Result<Option<String>, String> {
    selection.validate_page_count(count)?;
    let mut out = String::new();
    for (i, page) in selection.pages(count).enumerate() {
        let Some(text) = page_text(page) else { return Ok(None) };
        append_selection_page(&mut out, selection, page, text, i > 0)?;
    }
    Ok(Some(out))
}

impl Document {
    /// Standard-security copy permission (Table 22 bit 5); owner authentication lifts it.
    /// Fails closed: an encrypted document whose security handler could not be read for
    /// editing (the renderer opened it, the editor did not) has no established permissions,
    /// so its text is not extractable.
    pub fn allows_copying(&self) -> bool {
        match self.permissions() {
            Some(p) => p.copy(),
            None => !(self.info.encrypted && self.editor.is_none()),
        }
    }

    /// The copy refusal shared by every text-returning path.
    pub fn require_copying(&self) -> Result<(), String> {
        if self.allows_copying() { Ok(()) } else { Err(COPY_DENIED.into()) }
    }

    pub fn reader_paragraph_selection(&self, page: usize, glyph: usize) -> Result<TextSelection, String> {
        if !self.allows_copying() {
            return Err(COPY_DENIED.into());
        }
        if page >= self.info.pages.len() {
            return Err("paragraph page is out of range".into());
        }
        let config = RenderConfig { password: self.password.as_deref().map(std::sync::Arc::from), ..Default::default() };
        let mut renderer = PageRenderer::new(self.bytes.clone(), config);
        let result = renderer.render(RenderRequest { page, kind: RequestKind::Text, tile: None, scale: 1.0, tag: 0 });
        let text = result.text.ok_or_else(|| format!("page text could not be read: {}", result.error.unwrap_or_default()))?;
        let range = paragraph_at(&text, glyph).ok_or_else(|| "paragraph glyph is out of range".to_string())?;
        Ok(TextSelection::Range {
            anchor: TextPosition { page, glyph: range.start },
            head: TextPosition { page, glyph: range.end.saturating_sub(1) },
        })
    }

    /// Read-only, permission-checked selection extraction shared by CLI/MCP and the reader.
    /// Pages are interpreted sequentially so document-wide copying does not retain every
    /// page's glyph layer in addition to the bounded output.
    pub fn selected_reader_text(&self, selection: TextSelection) -> Result<String, String> {
        if !self.allows_copying() {
            return Err(COPY_DENIED.into());
        }
        let count = self.info.pages.len();
        selection.validate_page_count(count)?;
        let config = RenderConfig { password: self.password.as_deref().map(std::sync::Arc::from), ..Default::default() };
        let mut renderer = PageRenderer::new(self.bytes.clone(), config);
        let mut out = String::new();
        for (i, page) in selection.pages(count).enumerate() {
            let result = renderer.render(RenderRequest { page, kind: RequestKind::Text, tile: None, scale: 1.0, tag: 0 });
            let text =
                result.text.ok_or_else(|| format!("page {} text could not be read: {}", page.saturating_add(1), result.error.unwrap_or_default()))?;
            append_selection_page(&mut out, selection, page, &text, i > 0)?;
        }
        Ok(out)
    }
}

/// Conservative geometric paragraph selection for untagged text. Adjacent reading-order
/// lines join only with overlapping measure, comparable height and ordinary line spacing.
/// Columns, separated headings and large paragraph gaps remain separate. This does not infer
/// semantic paragraph boundaries hidden in an untagged stream.
pub fn paragraph_at(text: &PageText, glyph: usize) -> Option<Range<usize>> {
    let (mut first, mut last) = text.line_at(glyph)?;
    let start = text.glyphs.get(first)?.rect;
    let end = text.glyphs.get(last)?.rect;
    let vertical = (end[1] + end[3] - start[1] - start[3]).abs() > (end[0] + end[2] - start[0] - start[2]).abs();
    let bbox = |first: usize, last: usize| {
        text.glyphs.get(first..last.saturating_add(1)).map(|glyphs| {
            glyphs.iter().fold([f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY], |mut r, g| {
                r[0] = r[0].min(g.rect[0]);
                r[1] = r[1].min(g.rect[1]);
                r[2] = r[2].max(g.rect[2]);
                r[3] = r[3].max(g.rect[3]);
                r
            })
        })
    };
    let joined = |a: [f32; 4], b: [f32; 4]| {
        if !a.into_iter().chain(b).all(f32::is_finite) {
            return false;
        }
        let horizontal = |a: [f32; 4], b: [f32; 4]| {
            let h = (a[3] - a[1]).max(0.1);
            let k = (b[3] - b[1]).max(0.1);
            let overlap = a[2].min(b[2]) - a[0].max(b[0]);
            let gap = (b[1] - a[3]).max(a[1] - b[3]);
            overlap > (a[2] - a[0]).min(b[2] - b[0]).max(1.0) * 0.3 && gap > -h.min(k) * 0.3 && gap < h.max(k) * 0.9 && (h / k - 1.0).abs() < 0.4
        };
        if vertical { horizontal([a[1], a[0], a[3], a[2]], [b[1], b[0], b[3], b[2]]) } else { horizontal(a, b) }
    };
    while let Some(previous) = first.checked_sub(1).and_then(|i| text.line_at(i)) {
        let current = text.line_at(first)?;
        if !joined(bbox(previous.0, previous.1)?, bbox(current.0, current.1)?) {
            break;
        }
        first = previous.0;
    }
    // Use the last single line's bounds, not the growing paragraph box.
    while let Some(next) = text.line_at(last.saturating_add(1)) {
        let current = text.line_at(last)?;
        if !joined(bbox(current.0, current.1)?, bbox(next.0, next.1)?) {
            break;
        }
        last = next.1;
    }
    Some(first..last.saturating_add(1))
}

#[cfg(test)]
mod tests;
