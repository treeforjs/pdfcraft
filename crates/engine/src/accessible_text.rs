//! Save As ▸ Accessible Text: a tagged document's text in logical structure order, as a screen
//! reader would read it (from [`a11y::reading_blocks`]): each element's actual text, else its
//! alternate text, else the text of its marked content; a blank line between blocks (one line
//! break between consecutive list items or table rows, tabs between table cells); artifacts
//! (running headers and footers, page numbers) left out.

use std::collections::HashMap;
use std::sync::Arc;

use pdfcraft_render::{PageRenderer, PageText, RenderConfig, RenderRequest, RequestKind};

use crate::{Document, a11y};

/// The accessible text stops growing past this many bytes (and says it was truncated).
pub const MAX_ACCESSIBLE_TEXT_BYTES: usize = 64 << 20;

/// The result of [`Document::accessible_text`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AccessibleText {
    /// The text, ending with a line break unless empty.
    pub text: String,
    /// Blocks written (paragraphs, headings, list items, table rows, figures, captions).
    pub blocks: usize,
    pub headings: usize,
    pub list_items: usize,
    pub table_rows: usize,
    /// Figures and formulas written (with their alternate text, else the text inside them).
    pub figures: usize,
    /// Pages whose text was read.
    pub pages_read: usize,
    /// Pages that could not be read (their marked content is left out).
    pub unreadable_pages: usize,
    /// The structure tree was too large to read in full, or the text reached
    /// [`MAX_ACCESSIBLE_TEXT_BYTES`]: later text is missing.
    pub truncated: bool,
}

/// One page's text with its glyphs by MCID.
struct Page {
    text: Arc<PageText>,
    by_mcid: HashMap<i64, Vec<usize>>,
}

impl Document {
    /// Save As ▸ Accessible Text: the text of the structure tree in logical order. Errors when
    /// the document is not tagged, has an empty structure tree or does not allow copying text.
    pub fn accessible_text(&self) -> Result<AccessibleText, String> {
        let tags = self.tags();
        if !tags.exists {
            return Err("the document is not tagged: it has no structure tree to read accessible text from (add tags first)".into());
        }
        if tags.nodes.is_empty() {
            return Err("the structure tree is empty: it has no elements to read accessible text from (add tags first)".into());
        }
        self.require_copying()?;
        let blocks = a11y::reading_blocks(&tags);
        let config = RenderConfig { password: self.password.as_deref().map(Arc::from), ..Default::default() };
        let mut renderer: Option<PageRenderer> = None;
        let mut pages: HashMap<usize, Option<Page>> = HashMap::new();
        let page_count = self.info.pages.len();
        let mut out = AccessibleText { truncated: tags.truncated, ..Default::default() };
        let mut previous: Option<a11y::BlockKind> = None;
        for block in &blocks {
            let mut s = String::new();
            // The last glyph written: (page, glyph index, line).
            let mut last: Option<(usize, usize, Option<u32>)> = None;
            for piece in &block.pieces {
                match piece {
                    a11y::Piece::Text(t) => {
                        space(&mut s);
                        s.push_str(t);
                        last = None;
                    }
                    a11y::Piece::Cell => {
                        s.truncate(s.trim_end_matches(' ').len());
                        s.push('\t');
                        last = None;
                    }
                    a11y::Piece::Content { page, mcid } => {
                        if *page >= page_count {
                            continue;
                        }
                        let entry = pages.entry(*page).or_insert_with(|| {
                            let r = renderer.get_or_insert_with(|| PageRenderer::new(self.bytes.clone(), config.clone()));
                            let text = r.render(RenderRequest { page: *page, kind: RequestKind::Text, scale: 1.0, ..Default::default() }).text?;
                            let mut by_mcid: HashMap<i64, Vec<usize>> = HashMap::new();
                            for (i, g) in text.glyphs.iter().enumerate() {
                                if let Some(m) = g.mcid {
                                    by_mcid.entry(i64::from(m)).or_default().push(i);
                                }
                            }
                            Some(Page { text, by_mcid })
                        });
                        let Some(p) = entry else { continue };
                        let Some(glyphs) = p.by_mcid.get(mcid) else { continue };
                        for &i in glyphs {
                            let Some(g) = p.text.glyphs.get(i) else { continue };
                            let line = p.text.line_of.get(i).copied();
                            // Glyphs that follow each other on the page are spaced as they are
                            // laid out; anything else is a word break.
                            let gap = match last {
                                Some((lp, li, ll)) if lp == *page && li.checked_add(1) == Some(i) => {
                                    p.text.space_before.get(i).copied().unwrap_or(false) || ll != line
                                }
                                _ => true,
                            };
                            if gap {
                                space(&mut s);
                            }
                            s.push_str(&g.text);
                            last = Some((*page, i, line));
                            if s.len() > MAX_ACCESSIBLE_TEXT_BYTES {
                                break;
                            }
                        }
                    }
                }
                if s.len() > MAX_ACCESSIBLE_TEXT_BYTES {
                    break;
                }
            }
            // Collapse runs of white space inside each cell.
            let s: String = s.split('\t').map(|cell| cell.split_whitespace().collect::<Vec<_>>().join(" ")).collect::<Vec<_>>().join("\t");
            let s = s.trim_matches('\t');
            if s.is_empty() {
                continue;
            }
            let sep = match (previous, block.kind) {
                (None, _) => "",
                (Some(a11y::BlockKind::ListItem), a11y::BlockKind::ListItem) | (Some(a11y::BlockKind::TableRow), a11y::BlockKind::TableRow) => "\n",
                _ => "\n\n",
            };
            if out.text.len().saturating_add(sep.len()).saturating_add(s.len()).saturating_add(1) > MAX_ACCESSIBLE_TEXT_BYTES {
                out.truncated = true;
                break;
            }
            out.text.push_str(sep);
            out.text.push_str(s);
            previous = Some(block.kind);
            out.blocks += 1;
            match block.kind {
                a11y::BlockKind::Heading(_) => out.headings += 1,
                a11y::BlockKind::ListItem => out.list_items += 1,
                a11y::BlockKind::TableRow => out.table_rows += 1,
                a11y::BlockKind::Figure => out.figures += 1,
                a11y::BlockKind::Paragraph | a11y::BlockKind::Caption => {}
            }
        }
        if !out.text.is_empty() {
            out.text.push('\n');
        }
        out.pages_read = pages.values().filter(|p| p.is_some()).count();
        out.unreadable_pages = pages.len().saturating_sub(out.pages_read);
        Ok(out)
    }
}

/// A word break before more text, unless there is one already (or nothing yet).
fn space(s: &mut String) {
    if !s.is_empty() && !s.ends_with([' ', '\t', '\n']) {
        s.push(' ');
    }
}
