//! Compare Files tools: `compare_files` (two files on disk) and `doc_compare` (two open
//! documents) share one structured JSON report ([`report_json`]) — the page map, the text changes and the pages
//! that look different.

use std::sync::Arc;

use pdfcraft_engine::DocId;
use pdfcraft_engine::compare::{CompareOptions, Kind, report_json};
use pdfcraft_engine::compare_view::{CompareView, Spot, ViewChange};
use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError, failed, write_atomic};

/// Changes listed in a tool result by default (the JSON report file has them all).
const DEFAULT_LIMIT: i64 = 500;

fn r2(r: &[f64; 4]) -> [f64; 4] {
    r.map(|v| (v * 100.0).round() / 100.0)
}

/// A 1-based `[first, last]` page range argument → 0-based inclusive.
fn range(a: &Args, key: &str) -> Result<Option<(usize, usize)>> {
    let Some(v) = a.opt_ints(key)? else { return Ok(None) };
    match v.as_slice() {
        [first, last] if *first >= 1 && first <= last => {
            let to = |n: i64| usize::try_from(n - 1).map_err(|_| ToolError::InvalidArgs(format!("{key} is out of range")));
            Ok(Some((to(*first)?, to(*last)?)))
        }
        _ => Err(ToolError::InvalidArgs(format!("{key} must be [first, last]: two 1-based page numbers, first ≤ last"))),
    }
}

/// The compare options from a tool's arguments; `visual` defaults to `visual_default`.
fn options(a: &Args, visual_default: bool) -> Result<CompareOptions> {
    let mut o = CompareOptions { old_pages: range(a, "old_pages")?, new_pages: range(a, "new_pages")?, ..Default::default() };
    o.visual = a.opt_bool("visual")?.unwrap_or(visual_default);
    if let Some(dpi) = a.opt_num("dpi")? {
        if !(18.0..=150.0).contains(&dpi) {
            return Err(ToolError::InvalidArgs("dpi must be between 18 and 150".into()));
        }
        o.dpi = dpi as f32;
    }
    if let Some(t) = a.opt_int("tolerance")? {
        o.tolerance = u8::try_from(t).map_err(|_| ToolError::InvalidArgs("tolerance must be between 0 and 255".into()))?;
    }
    if let Some(t) = a.opt_num("threshold")? {
        if !(0.0..=1.0).contains(&t) {
            return Err(ToolError::InvalidArgs("threshold must be between 0 and 1 (a share of the page's pixels)".into()));
        }
        o.threshold = t;
    }
    Ok(o)
}

impl Automation {
    pub(crate) fn compare_files(&mut self, a: &Args) -> Result<Value> {
        let opts = options(a, true)?;
        let limit = a.opt_int("limit")?.unwrap_or(DEFAULT_LIMIT).max(1) as usize;
        // Output paths are checked before any work is done.
        let out = |key: &str| a.opt_str(key)?.map(|p| self.resolve(p, true)).transpose();
        let (json_path, report_path, overlay_path) = (out("json_path")?, out("report_path")?, out("overlay_path")?);
        let old = self.open_for_compare(a, "old", "old_password")?;
        let new = match self.open_for_compare(a, "new", "new_password") {
            Ok(id) => id,
            Err(e) => {
                self.session.close(old);
                return Err(e);
            }
        };
        let result = (|| {
            let c = self.session.compare_files(old, new, &opts).map_err(failed)?;
            let mut v = self.session.compare_json(old, new, &c, &opts, limit);
            if let Some(p) = &json_path {
                let full = self.session.compare_json(old, new, &c, &opts, usize::MAX);
                let text = serde_json::to_vec_pretty(&full).map_err(failed)?;
                write_atomic(p, &text)?;
                v["json_path"] = json!(p.to_string_lossy());
            }
            if let Some(p) = &report_path {
                let bytes = self.session.compare_files_report(old, new, &c).map_err(failed)?;
                write_atomic(p, &bytes)?;
                v["report_path"] = json!(p.to_string_lossy());
            }
            if let Some(p) = &overlay_path {
                let bytes = self.session.compare_overlay(new, &c).map_err(failed)?;
                write_atomic(p, &bytes)?;
                v["overlay_path"] = json!(p.to_string_lossy());
            }
            Ok(v)
        })();
        self.session.close(old);
        self.session.close(new);
        result
    }

    /// Open the file named by argument `key` (with the password in `password_key`) for a
    /// comparison; the caller closes it.
    fn open_for_compare(&mut self, a: &Args, key: &str, password_key: &str) -> Result<DocId> {
        let path = self.resolve(a.str(key)?, false)?;
        let bytes = std::fs::read(&path).map_err(|e| failed(format!("{}: {e}", path.display())))?;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.session
            .open(name, Some(path.to_string_lossy().into_owned()), Arc::new(bytes), a.opt_str(password_key)?)
            .map_err(|e| failed(format!("{key}: {e}")))
    }

    pub(crate) fn doc_compare(&self, a: &Args) -> Result<Value> {
        let (old, new) = self.compare_ids(a)?;
        let opts = options(a, false)?;
        let limit = a.opt_int("limit")?.unwrap_or(DEFAULT_LIMIT).max(1) as usize;
        let c = self.session.compare_files(old, new, &opts).map_err(failed)?;
        let mut v = report_json(&c, &opts, limit);
        // The original flat fields, kept for existing callers.
        v["replaced"] = json!(c.text.count(Kind::Replaced));
        v["inserted"] = json!(c.text.count(Kind::Inserted));
        v["deleted"] = json!(c.text.count(Kind::Deleted));
        v["visual_regions"] =
            json!(c.visual.iter().flat_map(|p| p.regions.iter().map(move |r| json!({ "page": p.new_page + 1, "rect": r2(r) }))).collect::<Vec<_>>());
        Ok(v)
    }

    /// `doc_compare_view`: the side-by-side view's rows and changes, and the current change.
    pub(crate) fn doc_compare_view(&self, a: &Args) -> Result<Value> {
        let (old, new) = self.compare_ids(a)?;
        let opts = options(a, false)?;
        let limit = a.opt_int("limit")?.unwrap_or(DEFAULT_LIMIT).max(1) as usize;
        let forward = match a.opt_str("step")? {
            None => None,
            Some("next") => Some(true),
            Some("previous") => Some(false),
            Some(other) => return Err(ToolError::InvalidArgs(format!("step must be next or previous, not {other:?}"))),
        };
        let change = a.opt_int("change")?;
        let c = self.session.compare_files(old, new, &opts).map_err(failed)?;
        let v = CompareView::new(&c);
        let n = v.changes.len();
        let from = match change {
            None => None,
            Some(i) if i >= 1 && usize::try_from(i).is_ok_and(|i| i <= n) => usize::try_from(i - 1).ok(),
            Some(i) if n == 0 => return Err(ToolError::InvalidArgs(format!("change {i}: this comparison found no changes"))),
            Some(i) => return Err(ToolError::InvalidArgs(format!("change {i} is out of range: there are {n} changes (1 to {n})"))),
        };
        let current = match forward {
            Some(f) => v.step(from, f),
            None => from,
        };
        let page = |p: Option<usize>| p.map(|p| p + 1);
        let rows: Vec<Value> = v
            .rows
            .iter()
            .enumerate()
            .take(limit)
            .map(|(i, r)| json!({ "row": i + 1, "old_page": page(r.old), "new_page": page(r.new), "status": r.status.label() }))
            .collect();
        let spot = |s: &Option<Spot>| {
            s.as_ref().map(|s| json!({ "page": s.page + 1, "row": s.row + 1, "rects": s.rects.iter().map(r2).collect::<Vec<_>>(), "focus": s.focus.as_ref().map(r2) }))
        };
        let entry = |i: usize, ch: &ViewChange| json!({ "index": i + 1, "kind": ch.kind.name(), "old": spot(&ch.old), "new": spot(&ch.new) });
        let changes: Vec<Value> = v.changes.iter().enumerate().take(limit).map(|(i, ch)| entry(i, ch)).collect();
        let current = current.and_then(|i| v.changes.get(i).map(|ch| (i, ch))).map(|(i, ch)| {
            let mut e = entry(i, ch);
            e["scroll_to_row"] = json!(ch.row().map(|r| r + 1));
            if let Some(t) = ch.text.and_then(|t| c.text.changes.get(t)) {
                e["old_text"] = json!(t.old.text);
                e["new_text"] = json!(t.new.text);
            }
            e
        });
        Ok(json!({
            "rows": rows,
            "row_count": v.rows.len(),
            "changes": changes,
            "change_count": n,
            "truncated": v.rows.len() > limit || n > limit,
            "current": current,
        }))
    }
}
