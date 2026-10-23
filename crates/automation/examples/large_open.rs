//! Time to first page and peak memory on a very large synthetic document (`core.lazy-loading`).
//!
//! ```text
//! large_open generate OUT.pdf [--pages N (4000)] [--image-bytes B (60000)] [--linearized]
//! large_open measure FILE.pdf
//! ```
//!
//! Generates a deterministic, original PDF (no assets): `N` pages in a balanced page tree, each
//! with 30 lines of Helvetica text and its own uncompressed `B`-byte image, so the file is
//! roughly `N × B` bytes (the default is about 250 MB and 4 000 pages). With `--linearized` it
//! is rewritten with PdfCraft's own linearizing writer. `measure`, in a fresh process (so peak
//! RSS is the open's alone), drives
//! the same tool table as `pdfcraft-cli run` and MCP: `doc_open`, `page_render` of page 1 (96
//! dpi), `doc_open` of the same file again (warm), and `page_render` of the last page. Prints
//! one JSON object with milliseconds per phase, time to first page and peak RSS (`VmHWM`,
//! Linux). Unlike `cargo xtask perf` (the budget harness) it measures only opening.

use std::time::Instant;

use pdfcraft_automation::{Automation, Content};
use serde_json::{Value, json};

fn peak_rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// A deterministic byte sequence (64-bit LCG, Knuth's MMIX constants): image data that doesn't
/// compress, like a scan.
fn noise(n: usize) -> Vec<u8> {
    let mut state = 0x5eed_u64;
    (0..n)
        .map(|_| {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (state >> 56) as u8
        })
        .collect()
}

/// The synthetic document: catalog 1, root page tree 2, font 3, 50-kid intermediate nodes, then
/// (page, contents, image) per page.
fn build(pages: usize, image_bytes: usize) -> Vec<u8> {
    const FAN: usize = 50;
    let nodes = pages.div_ceil(FAN).max(1);
    let first_node = 4;
    let first_page = first_node + nodes;
    let width = 200;
    let height = (image_bytes / (3 * width)).max(1);
    let pixels = noise(width * height * 3);
    let mut out = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let total = first_page + pages * 3;
    let mut offsets = vec![0usize; total];
    let mut object = |out: &mut Vec<u8>, num: usize, body: &[u8]| {
        if let Some(slot) = offsets.get_mut(num) {
            *slot = out.len();
        }
        out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    };
    object(&mut out, 1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let kids: Vec<String> = (0..nodes).map(|k| format!("{} 0 R", first_node + k)).collect();
    object(&mut out, 2, format!("<< /Type /Pages /Count {pages} /Kids [{}] >>", kids.join(" ")).as_bytes());
    object(&mut out, 3, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    for k in 0..nodes {
        let members: Vec<String> = (k * FAN..((k + 1) * FAN).min(pages)).map(|i| format!("{} 0 R", first_page + i * 3)).collect();
        let body = format!("<< /Type /Pages /Parent 2 0 R /Count {} /Kids [{}] >>", members.len(), members.join(" "));
        object(&mut out, first_node + k, body.as_bytes());
    }
    for i in 0..pages {
        let (page, contents, image) = (first_page + i * 3, first_page + i * 3 + 1, first_page + i * 3 + 2);
        let parent = first_node + i / FAN;
        let mut text = String::from("q 200 0 0 100 300 600 cm /Im1 Do Q\n");
        for l in 0..30 {
            text.push_str(&format!("BT /F1 10 Tf 72 {} Td (Page {} line {l} with some words) Tj ET\n", 720 - l * 15, i + 1));
        }
        let body = format!(
            "<< /Type /Page /Parent {parent} 0 R /MediaBox [0 0 612 792] /Contents {contents} 0 R /Resources << /Font << /F1 3 0 R >> /XObject << /Im1 {image} 0 R >> >> >>"
        );
        object(&mut out, page, body.as_bytes());
        let mut stream = format!("<< /Length {} >>\nstream\n", text.len()).into_bytes();
        stream.extend_from_slice(text.as_bytes());
        stream.extend_from_slice(b"\nendstream");
        object(&mut out, contents, &stream);
        let mut stream = format!(
            "<< /Type /XObject /Subtype /Image /Width {width} /Height {height} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length {} >>\nstream\n",
            pixels.len()
        )
        .into_bytes();
        stream.extend_from_slice(&pixels);
        stream.extend_from_slice(b"\nendstream");
        object(&mut out, image, &stream);
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
    for offset in offsets.iter().skip(1) {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes());
    out
}

fn ms(t: Instant) -> f64 {
    (t.elapsed().as_secs_f64() * 10_000.0).round() / 10.0
}

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: large_open generate OUT.pdf [--pages N] [--image-bytes B] [--linearized] | large_open measure FILE.pdf";
    let mode = args.next().ok_or(usage)?;
    let file = args.next().ok_or(usage)?;
    if mode == "generate" {
        let (mut pages, mut image_bytes, mut linearized) = (4000usize, 60_000usize, false);
        while let Some(a) = args.next() {
            let mut number = |what: &str| args.next().and_then(|v| v.parse::<usize>().ok()).ok_or(format!("{what} needs a number"));
            match a.as_str() {
                "--pages" => pages = number("--pages")?.clamp(1, 1_000_000),
                "--image-bytes" => image_bytes = number("--image-bytes")?.clamp(600, 50_000_000),
                "--linearized" => linearized = true,
                other => return Err(format!("unknown argument {other:?}; {usage}")),
            }
        }
        let mut bytes = build(pages, image_bytes);
        if linearized {
            let doc = pdfcraft_cos::Document::open(std::sync::Arc::new(bytes)).map_err(|e| e.to_string())?;
            bytes = pdfcraft_cos::write_linearized(&doc, &pdfcraft_cos::SaveOptions::default()).map_err(|e| e.to_string())?;
        }
        std::fs::write(&file, &bytes).map_err(|e| format!("{file}: {e}"))?;
        println!("{}", json!({ "file": file, "pages": pages, "file_bytes": bytes.len(), "linearized": linearized }));
        return Ok(());
    }
    if mode != "measure" {
        return Err(usage.into());
    }
    let mut auto = Automation::new();
    let mut phases = serde_json::Map::new();
    let mut phase = |auto: &mut Automation, name: &str, tool: &str, args: Value| -> Result<(f64, Value), String> {
        let t = Instant::now();
        let out = auto.call(tool, &args).map_err(|e| format!("{name} ({tool}): {e}"))?;
        let elapsed = ms(t);
        phases.insert(name.into(), json!({ "ms": elapsed, "peak_rss_kib": peak_rss_kib() }));
        let value = out
            .iter()
            .find_map(|c| match c {
                Content::Json(v) => Some(v.clone()),
                _ => None,
            })
            .unwrap_or(Value::Null);
        Ok((elapsed, value))
    };
    let rss_before = peak_rss_kib();
    let (open_ms, opened) = phase(&mut auto, "open", "doc_open", json!({ "path": file }))?;
    let doc = opened["doc"].as_u64().ok_or("doc_open returned no document id")?;
    let pages = opened["pages"].as_u64().ok_or("doc_open returned no page count")?;
    let (first_ms, _) = phase(&mut auto, "render_first_page", "page_render", json!({ "doc": doc, "page": 1, "dpi": 96 }))?;
    let first_page_peak = peak_rss_kib();
    let (_, opened) = phase(&mut auto, "open_again", "doc_open", json!({ "path": file }))?;
    let again = opened["doc"].as_u64().ok_or("doc_open returned no document id")?;
    phase(&mut auto, "render_last_page", "page_render", json!({ "doc": again, "page": pages, "dpi": 96 }))?;
    let report = json!({
        "pages": pages,
        "file_bytes": std::fs::metadata(&file).map(|m| m.len()).ok(),
        "time_to_first_page_ms": ((open_ms + first_ms) * 10.0).round() / 10.0,
        "peak_rss_kib_before_open": rss_before,
        "peak_rss_kib_at_first_page": first_page_peak,
        "phases": phases,
        "peak_rss_kib": peak_rss_kib(),
    });
    println!("{report}");
    Ok(())
}
