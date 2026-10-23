//! Help (`help_shortcuts`, `help_guide`): the keyboard reference and the bundled user guide of
//! [`pdfcraft_engine::help`], the same ones Help → Keyboard shortcuts and Help → User guide show.

use pdfcraft_engine::help::{GUIDE, KeyRow, guide_page, query_terms, row_matches, search_guide};
use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError};

fn bad(m: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs(m.into())
}

fn row_json(r: &KeyRow, platform: &str) -> Value {
    let mut v = json!({ "section": r.section, "action": r.action, "command": r.command });
    if platform != "other" {
        v["mac"] = json!(r.mac);
    }
    if platform != "mac" {
        v["other"] = json!(r.other);
    }
    v
}

impl Automation {
    /// The shortcuts, optionally searched (`query`) and for one platform (`platform`).
    pub(crate) fn help_shortcuts(&self, a: &Args) -> Result<Value> {
        let platform = a.opt_str("platform")?.unwrap_or("both");
        if !matches!(platform, "mac" | "other" | "both") {
            return Err(bad(format!("platform must be mac, other or both, not {platform:?}")));
        }
        let terms = query_terms(a.opt_str("query")?.unwrap_or(""));
        let rows: Vec<Value> =
            pdfcraft_engine::help::key_reference().iter().filter(|r| row_matches(r, &terms, &[])).map(|r| row_json(r, platform)).collect();
        Ok(json!({ "count": rows.len(), "shortcuts": rows }))
    }

    /// The guide's pages, one page's Markdown (`page`), or the lines matching `query`.
    pub(crate) fn help_guide(&self, a: &Args) -> Result<Value> {
        let pages = || GUIDE.iter().map(|p| json!({ "page": p.slug, "title": p.title() })).collect::<Vec<_>>();
        match (a.opt_str("page")?, a.opt_str("query")?) {
            (Some(_), Some(_)) => Err(bad("give page or query, not both")),
            (Some(slug), None) => {
                let p = guide_page(slug).ok_or_else(|| {
                    let all: Vec<&str> = GUIDE.iter().map(|p| p.slug).collect();
                    bad(format!("unknown page {:?}; the pages are {}", slug.chars().take(80).collect::<String>(), all.join(", ")))
                })?;
                Ok(json!({ "page": p.slug, "title": p.title(), "markdown": p.markdown }))
            }
            (None, Some(q)) => {
                let limit = match a.opt_int("limit")? {
                    Some(n) if !(1..=500).contains(&n) => return Err(bad("limit must be 1 to 500")),
                    Some(n) => usize::try_from(n).unwrap_or(50),
                    None => 50,
                };
                let hits: Vec<Value> =
                    search_guide(q, limit).into_iter().map(|h| json!({ "page": h.slug, "line": h.line, "text": h.text })).collect();
                Ok(json!({ "count": hits.len(), "matches": hits }))
            }
            (None, None) => Ok(json!({ "pages": pages() })),
        }
    }
}
