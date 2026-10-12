use crate::{Args, Automation, Result, ToolError, failed};
use pdfcraft_engine::reader_selection::{TextPosition, TextSelection};
use serde_json::{Value, json};

impl Automation {
    pub(crate) fn text_selection(&self, a: &Args) -> Result<Value> {
        let doc = self.doc(a)?;
        let mode = a.opt_str("mode")?.unwrap_or("range");
        let position = |page: &str, glyph: &str| -> Result<TextPosition> {
            let index = |key: &str| -> Result<usize> {
                a.int(key)?
                    .checked_sub(1)
                    .and_then(|n| usize::try_from(n).ok())
                    .ok_or_else(|| ToolError::InvalidArgs(format!("{key} must be a positive one-based index")))
            };
            Ok(TextPosition { page: index(page)?, glyph: index(glyph)? })
        };
        let has = |keys: &[&str]| keys.iter().any(|key| a.get(key).is_some());
        let selection = match mode {
            "all" => {
                if has(&["start_page", "start_glyph", "end_page", "end_glyph"]) {
                    return Err(ToolError::InvalidArgs("all mode does not take endpoints".into()));
                }
                TextSelection::All
            }
            "paragraph" => {
                if has(&["end_page", "end_glyph"]) {
                    return Err(ToolError::InvalidArgs("paragraph mode takes only start_page and start_glyph".into()));
                }
                let at = position("start_page", "start_glyph")?;
                doc.reader_paragraph_selection(at.page, at.glyph).map_err(failed)?
            }
            "range" => TextSelection::Range { anchor: position("start_page", "start_glyph")?, head: position("end_page", "end_glyph")? },
            _ => return Err(ToolError::InvalidArgs("mode must be range, paragraph, or all".into())),
        };
        let text = doc.selected_reader_text(selection).map_err(failed)?;
        let endpoints = selection
            .ordered()
            .map(|(start, end)| json!({"start_page":start.page+1,"start_glyph":start.glyph+1,"end_page":end.page+1,"end_glyph":end.glyph+1}));
        Ok(json!({"doc":doc.id.0,"text":text,"mode":mode,"range":endpoints}))
    }
}
