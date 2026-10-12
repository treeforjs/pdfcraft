//! Headless UI performance probe for `cargo xtask perf` (parity `misc.performance-budgets`,
//! `view.large-document-performance`).
//!
//! ```text
//! perf_ui <file.pdf> <page-count> [--panel NAME]
//! ```
//!
//! Runs the real PdfCraft shell under egui_kittest (no window, no GPU frame submission) and
//! measures, in this fresh process:
//! - `open_ms`: `PdfCraftApp::open_bytes` on the file's bytes;
//! - `first_frame_ms`: the first UI frame after opening;
//! - `settle_ms`: from opening until every demanded render is in (visible pages, their prefetch
//!   neighbours and thumbnails), i.e. the first page is on screen at full quality;
//! - `smooth_scroll`: 300 frames of continuous scrolling at a tenth of a page per frame, and
//!   `jump_scroll`: 200 frames jumping 2.5 pages per frame. Frames run back to back while the
//!   render workers keep working; each frame's time is the UI's own work for it, including
//!   taking finished rasters. Reports median, 95th percentile, worst and frames over 16.7 ms;
//! - `navigate_ms`: for jumps to far pages, the time until that page's view is settled;
//! - `peak_rss_kib`: `VmHWM` from `/proc/self/status` (Linux only).
//!
//! `--panel NAME` sets the right-hand panel after opening (as the `--panel` app option; `none`
//! closes it). By default the app chooses, as for a user: Comments when the file has comments.
//!
//! Prints one JSON object on stdout.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use pdfcraft_ui_egui::PdfCraftApp;
use serde_json::json;

fn peak_rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Median, 95th percentile, worst and the number of frames over a 60 fps frame (16.7 ms).
fn frame_stats(mut frames: Vec<f64>) -> serde_json::Value {
    frames.sort_by(f64::total_cmp);
    let at = |q: f64| frames.get(((frames.len().saturating_sub(1)) as f64 * q).round() as usize).copied().unwrap_or(0.0);
    json!({
        "frames": frames.len(),
        "median_ms": at(0.5),
        "p95_ms": at(0.95),
        "max_ms": frames.last().copied().unwrap_or(0.0),
        "over_16_7ms": frames.iter().filter(|f| **f > 1000.0 / 60.0).count(),
    })
}

/// Step until no render has been pending for three frames in a row (layout can take a frame or
/// two to demand anything), or `limit` passes. Returns the time until the first of those quiet
/// frames and whether it settled.
fn settle(h: &mut Harness<'_, PdfCraftApp>, limit: Duration) -> (Duration, bool) {
    let t = Instant::now();
    let mut quiet_since = None;
    let mut quiet = 0;
    while t.elapsed() < limit {
        h.step();
        if h.state().render_pending() {
            quiet = 0;
            quiet_since = None;
            std::thread::sleep(Duration::from_millis(2));
        } else {
            quiet_since.get_or_insert(t.elapsed());
            quiet += 1;
            if quiet >= 3 {
                return (quiet_since.unwrap_or_else(|| t.elapsed()), true);
            }
        }
    }
    (t.elapsed(), false)
}

fn main() -> Result<(), String> {
    let usage = "usage: perf_ui <file.pdf> <page-count> [--panel NAME]";
    let file = std::env::args().nth(1).ok_or(usage)?;
    let pages: usize = std::env::args().nth(2).and_then(|n| n.parse().ok()).filter(|n| *n > 0).ok_or(usage)?;
    let panel = match (std::env::args().nth(3).as_deref(), std::env::args().nth(4)) {
        (Some("--panel"), Some(p)) => Some(p),
        (None, _) => None,
        _ => return Err(usage.into()),
    };
    let chosen_panel = panel.clone();
    let bytes = std::fs::read(&file).map_err(|e| format!("{file}: {e}"))?;
    let open_time = Arc::new(Mutex::new(None::<Result<Duration, String>>));
    let slot = Arc::clone(&open_time);
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        let _ = app.set_option("language", "en");
        let t = Instant::now();
        let mut result = app.open_bytes("perf.pdf", None, bytes).map(|()| t.elapsed());
        if let (Ok(_), Some(p)) = (&result, &panel)
            && let Err(e) = app.set_option("panel", p)
        {
            result = Err(e);
        }
        if let Ok(mut s) = slot.lock() {
            *s = Some(result);
        }
        app
    });
    let open = open_time.lock().map_err(|_| "poisoned")?.take().ok_or("the app was not built")??;
    let t = Instant::now();
    h.step();
    let first_frame = t.elapsed();
    let (settle_time, settled) = settle(&mut h, Duration::from_secs(120));
    // Opening, the first frame and waiting for the renders: what a user waits for the first page.
    let settle_ms = ms(open + first_frame + settle_time);
    if h.state().views.is_empty() {
        return Err("no document view".into());
    }
    // Proof that settling meant pixels on screen, not an empty queue before the first layout.
    let first_rasters = h.state().views.first().map_or(0, |v| v.raster_memory().page_count + v.raster_memory().tile_count);

    let mut smooth = Vec::new();
    for f in 0..300usize {
        let page = (f / 10) % pages;
        if let Some(v) = h.state_mut().views.first_mut() {
            v.goto = Some((page, (f % 10) as f32 / 10.0));
        }
        let t = Instant::now();
        h.step();
        smooth.push(ms(t.elapsed()));
    }
    let mut jump = Vec::new();
    for f in 0..200usize {
        if let Some(v) = h.state_mut().views.first_mut() {
            v.goto = Some(((f * 5) / 2 % pages, 0.3));
        }
        let t = Instant::now();
        h.step();
        jump.push(ms(t.elapsed()));
    }
    settle(&mut h, Duration::from_secs(120));

    let mut navigate = Vec::new();
    let mut all_settled = settled;
    for target in [pages / 2, pages - 1, pages / 4, (pages * 3) / 4, 0] {
        if let Some(v) = h.state_mut().views.first_mut() {
            v.goto = Some((target, 0.0));
        }
        let (d, ok) = settle(&mut h, Duration::from_secs(120));
        all_settled &= ok;
        navigate.push(ms(d));
    }
    let raster = h.state().views.first().map(|v| v.raster_memory());

    let report = json!({
        "probe": "ui",
        "pages": pages,
        "panel": chosen_panel,
        "open_ms": ms(open),
        "first_frame_ms": ms(first_frame),
        "settle_ms": settle_ms,
        "first_rasters": first_rasters,
        "smooth_scroll": frame_stats(smooth),
        "jump_scroll": frame_stats(jump),
        "navigate_ms": navigate,
        "all_settled": all_settled,
        "raster_memory": raster,
        "peak_rss_kib": peak_rss_kib(),
    });
    println!("{report}");
    Ok(())
}
