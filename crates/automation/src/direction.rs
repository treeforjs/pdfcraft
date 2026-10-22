//! `text_direction`: right-to-left and CJK text editing support for agents: a paragraph's (or
//! given text's) direction, display order, and where the Edit text caret moves on screen.

use pdfcraft_engine::bidi;
use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError, withheld};

/// The longest text examined; paragraph edits are capped at the same length.
const MAX_TEXT: usize = 1_000_000;
/// The longest text whose full display order is listed.
const MAX_ORDER: usize = 10_000;

fn bad(m: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs(m.into())
}

/// The scripts of `text` an editor cares about, in order of appearance.
fn scripts(text: &str) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for c in text.chars() {
        let script = match u32::from(c) {
            0x0590..=0x05FF | 0xFB1D..=0xFB4F => "hebrew",
            0x0600..=0x06FF | 0x0750..=0x077F | 0x0870..=0x08FF | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF => "arabic",
            0x3040..=0x30FF | 0x31F0..=0x31FF => "kana",
            0x1100..=0x11FF | 0x3130..=0x318F | 0xAC00..=0xD7AF => "hangul",
            0x2E80..=0x2FFF | 0x3400..=0x9FFF | 0xF900..=0xFAFF | 0x2_0000..=0x3_FFFF => "han",
            _ if c.is_ascii_alphabetic() || ('\u{00C0}'..='\u{024F}').contains(&c) => "latin",
            _ => continue,
        };
        if !out.contains(&script) {
            out.push(script);
        }
    }
    out
}

impl Automation {
    pub(crate) fn text_direction(&self, a: &Args) -> Result<Value> {
        let (text, vertical, copy) = match (a.opt_str("text")?, a.opt_int("paragraph")?) {
            (Some(_), Some(_)) => return Err(bad("pass either text or doc, page and paragraph, not both")),
            (Some(text), None) => (text.to_owned(), false, true),
            (None, Some(k)) => {
                let page = self.page(a)?;
                let doc = self.doc(a)?;
                let blocks = doc.text_blocks(page);
                let b = usize::try_from(k)
                    .ok()
                    .and_then(|k| k.checked_sub(1))
                    .and_then(|i| blocks.get(i))
                    .ok_or_else(|| bad(format!("paragraph {k} is out of range: page {} has {} paragraphs", page + 1, blocks.len())))?;
                (b.text.clone(), b.vertical, doc.allows_copying())
            }
            (None, None) => return Err(bad("pass text, or doc, page and paragraph (from text_paragraphs)")),
        };
        let chars = text.chars().take(MAX_TEXT + 1).count();
        if chars > MAX_TEXT {
            return Err(bad(format!("text is longer than {MAX_TEXT} characters")));
        }
        let rtl = match a.opt_str("direction")? {
            None | Some("auto") => None,
            Some("rtl") => Some(true),
            Some("ltr") => Some(false),
            Some(other) => return Err(bad(format!("direction must be auto, ltr or rtl, not {other:?}"))),
        };
        let base = rtl.unwrap_or_else(|| bidi::base_rtl(&text));
        let mut out = json!({
            "direction": if base { "rtl" } else { "ltr" },
            "vertical": vertical,
            "scripts": scripts(&text),
            "length": chars,
            "display": copy.then(|| bidi::visual_text(&text, rtl)),
        });
        if chars <= MAX_ORDER && copy {
            out["display_order"] = json!(bidi::visual_order(&text, rtl));
        }
        if let Some(caret) = a.opt_int("caret")? {
            let caret = usize::try_from(caret)
                .ok()
                .filter(|c| *c <= chars)
                .ok_or_else(|| bad(format!("caret must be from 0 to {chars} (Unicode scalar offsets)")))?;
            let right = match a.opt_str("move")? {
                Some("right") => true,
                Some("left") => false,
                None => return Err(bad("caret needs move: left or right")),
                Some(other) => return Err(bad(format!("move must be left or right, not {other:?}"))),
            };
            let steps = a.opt_int("steps")?.unwrap_or(1);
            if !(1..=10_000).contains(&steps) {
                return Err(bad("steps must be from 1 to 10000"));
            }
            let mut at = caret;
            let mut path = Vec::new();
            for _ in 0..steps {
                at = bidi::caret_move(&text, at, right, rtl);
                path.push(at);
            }
            out["caret"] = json!(at);
            out["caret_path"] = json!(path);
        }
        Ok(withheld(out, copy))
    }
}
