//! Headless performance probe for `cargo xtask perf` (parity `misc.performance-budgets`).
//!
//! ```text
//! perf_probe <file.pdf> <find-phrase>
//! ```
//!
//! Drives the same automation tool table as `pdfcraft-cli run` and MCP, timing each phase in this
//! fresh process: `doc_open`, `page_render` of the first and the last page (96 dpi, PNG encoding
//! included, as an agent receives it), `text_extract` of every page, `text_find`, an incremental
//! `doc_save` in place after a metadata edit, and a full `doc_save` to a new path. The file is
//! modified in place, so the caller passes a scratch copy. Prints one JSON object on stdout with
//! milliseconds per phase and the process's peak resident set (`VmHWM` from `/proc/self/status`,
//! Linux only) after each phase. `xtask perf` runs it several times per document in new processes.

use std::time::Instant;

use pdfcraft_automation::{Automation, Content};
use serde_json::{Value, json};

/// Peak resident set of this process in KiB (`VmHWM`), where the OS reports it.
fn peak_rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

fn json_of(content: &[Content]) -> Value {
    content
        .iter()
        .find_map(|c| match c {
            Content::Json(v) => Some(v.clone()),
            _ => None,
        })
        .unwrap_or(Value::Null)
}

struct Probe {
    auto: Automation,
    phases: serde_json::Map<String, Value>,
}

impl Probe {
    /// Run one tool call as a named phase; returns its content.
    fn phase(&mut self, name: &str, tool: &str, args: Value) -> Result<Vec<Content>, String> {
        let t = Instant::now();
        let out = self.auto.call(tool, &args).map_err(|e| format!("{name} ({tool}): {e}"))?;
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        self.phases.insert(name.into(), json!({ "ms": ms, "peak_rss_kib": peak_rss_kib() }));
        Ok(out)
    }
}

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: perf_probe <file.pdf> <find-phrase>";
    let file = args.next().ok_or(usage)?;
    let phrase = args.next().ok_or(usage)?;
    let full_out = format!("{file}.full.pdf");
    let start = Instant::now();
    let mut p = Probe { auto: Automation::new(), phases: serde_json::Map::new() };

    let opened = json_of(&p.phase("open", "doc_open", json!({ "path": file }))?);
    let doc = opened["doc"].as_u64().ok_or("doc_open returned no document id")?;
    let pages = opened["pages"].as_u64().ok_or("doc_open returned no page count")?;

    let first = p.phase("render_first_page", "page_render", json!({ "doc": doc, "page": 1, "dpi": 96 }))?;
    let png_bytes: usize = first.iter().map(|c| if let Content::Png { data, .. } = c { data.len() } else { 0 }).sum();
    p.phase("render_last_page", "page_render", json!({ "doc": doc, "page": pages, "dpi": 96 }))?;

    let text = json_of(&p.phase("extract_all_text", "text_extract", json!({ "doc": doc }))?);
    let text_chars = text.to_string().len();
    let found = json_of(&p.phase("find", "text_find", json!({ "doc": doc, "query": phrase }))?);
    let matches = found["matches"].as_array().map_or(0, Vec::len);
    if matches == 0 {
        return Err(format!("text_find found no match for {phrase:?}: {found}"));
    }

    p.auto.call("doc_set_info", &json!({ "doc": doc, "key": "Title", "value": "perf probe" })).map_err(|e| e.to_string())?;
    let inc = json_of(&p.phase("save_incremental", "doc_save", json!({ "doc": doc }))?);
    if inc["incremental"] != json!(true) {
        return Err(format!("save in place was not incremental: {inc}"));
    }
    let full = json_of(&p.phase("save_full", "doc_save", json!({ "doc": doc, "path": full_out }))?);
    if full["incremental"] != json!(false) {
        return Err(format!("save to a new path was not a full rewrite: {full}"));
    }

    let size = |f: &str| std::fs::metadata(f).map(|m| m.len()).ok();
    let report = json!({
        "probe": "engine",
        "pages": pages,
        "phases": p.phases,
        "total_ms": start.elapsed().as_secs_f64() * 1000.0,
        "peak_rss_kib": peak_rss_kib(),
        "first_page_png_bytes": png_bytes,
        "text_json_bytes": text_chars,
        "find_matches": matches,
        "incremental_file_bytes": size(&file),
        "full_file_bytes": size(&full_out),
    });
    println!("{report}");
    Ok(())
}
