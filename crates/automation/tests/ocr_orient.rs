//! End-to-end automatic orientation and deskew through the OCR tools, on original synthetic
//! scans: a PdfCraft-made text page rendered at 150 dpi, turned by 0/90/180/270° and skewed by
//! ±3/±7° (bilinear, written by this file independently of the library's own rotation code),
//! and wrapped as an image-only PDF at 72 dpi (one pixel per point).
//!
//! Ground truth is the source page's own glyph boxes carried through the same turn. Each case
//! asserts that the recognized words come out in reading order, that the searchable text found
//! in the saved layer lies on the visible word in the unchanged image, and that the page image
//! is untouched. Model-backed: set PDFCRAFT_MODELS_REQUIRED=1 so missing models fail.
use pdfcraft_automation::{Automation, Content};
use pdfcraft_engine::{Session, export};
use pdfcraft_render::{PageRenderer, RenderConfig, RenderRequest, RequestKind};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};

const TEXT: &str = "Searchable scans keep the original page image.\nRecognized words are placed over the visible text.\nOrientation and skew are measured before reading.\nEvery fixture here was generated for this test.";
const DPI: f64 = 150.0;

fn result(auto: &mut Automation, name: &str, args: Value) -> Value {
    let mut reply = auto.call(name, &args).unwrap_or_else(|e| panic!("{name}: {e}"));
    match reply.remove(0) {
        Content::Json(value) => value,
        other => panic!("expected JSON, got {other:?}"),
    }
}

fn root(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("pdfcraft-ocr-orient-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn normalize(word: &str) -> String {
    word.chars().filter(char::is_ascii_alphanumeric).flat_map(char::to_lowercase).collect()
}

/// A ground-truth word: its text and its box in source-image pixels [x0, y0, x1, y1].
struct Truth {
    text: String,
    rect: [f64; 4],
}

/// The source page (PdfCraft's own text page), its pixels at `DPI`, and its words.
fn source() -> (u32, u32, Vec<u8>, Vec<Truth>) {
    let session = Session::new();
    let bytes = session.create_from_text("Orientation fixture", TEXT).unwrap();
    let mut renderer = PageRenderer::new(bytes, RenderConfig::default());
    let shot = renderer.render(RenderRequest { scale: (DPI / 72.0) as f32, ..Default::default() });
    assert!(shot.error.is_none());
    let text = renderer.render(RenderRequest { kind: RequestKind::Text, scale: 1.0, ..Default::default() }).text.unwrap();
    let k = DPI / 72.0;
    let mut words: Vec<Truth> = Vec::new();
    for (i, g) in text.glyphs.iter().enumerate() {
        if g.text.trim().is_empty() {
            continue;
        }
        let r = g.rect.map(|v| f64::from(v) * k);
        let starts = i == 0 || text.space_before[i] || text.line_of[i] != text.line_of[i - 1] || text.glyphs[i - 1].text.trim().is_empty();
        match words.last_mut() {
            Some(w) if !starts => {
                w.text.push_str(&g.text);
                w.rect = [w.rect[0].min(r[0]), w.rect[1].min(r[1]), w.rect[2].max(r[2]), w.rect[3].max(r[3])];
            }
            _ => words.push(Truth { text: g.text.clone(), rect: r }),
        }
    }
    let words: Vec<Truth> = words.into_iter().filter(|w| !normalize(&w.text).is_empty()).collect();
    assert_eq!(words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" "), TEXT.replace('\n', " "), "fixture ground truth");
    (shot.width, shot.height, shot.rgba.to_vec(), words)
}

fn cos_sin(degrees: f64) -> (f64, f64) {
    let d = degrees.rem_euclid(360.0);
    [(0.0, (1.0, 0.0)), (90.0, (0.0, 1.0)), (180.0, (-1.0, 0.0)), (270.0, (0.0, -1.0))]
        .into_iter()
        .find(|(q, _)| *q == d)
        .map_or((d.to_radians().cos(), d.to_radians().sin()), |(_, cs)| cs)
}

/// The source turned `angle` degrees counterclockwise on a white canvas that holds it all, and
/// the map from source pixels to the turned image's.
struct Turned {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    angle: f64,
    centers: [f64; 4],
}

impl Turned {
    fn new(w: u32, h: u32, rgba: &[u8], angle: f64) -> Turned {
        let (c, s) = cos_sin(angle);
        let (fw, fh) = (f64::from(w), f64::from(h));
        let (ow, oh) = ((fw * c.abs() + fh * s.abs() - 1e-6).ceil() as u32, (fw * s.abs() + fh * c.abs() - 1e-6).ceil() as u32);
        let centers = [fw / 2.0, fh / 2.0, f64::from(ow) / 2.0, f64::from(oh) / 2.0];
        let mut out = vec![255u8; (ow * oh * 4) as usize];
        for j in 0..oh {
            for i in 0..ow {
                // Inverse of a counterclockwise turn (y down): back to the source.
                let (dx, dy) = (f64::from(i) + 0.5 - centers[2], f64::from(j) + 0.5 - centers[3]);
                let (x, y) = (c * dx - s * dy + centers[0] - 0.5, s * dx + c * dy + centers[1] - 0.5);
                let (x0, y0) = (x.floor(), y.floor());
                let (fx, fy) = (x - x0, y - y0);
                for ch in 0..3 {
                    let at = |xx: f64, yy: f64| {
                        if xx < 0.0 || yy < 0.0 || xx >= fw || yy >= fh {
                            255.0
                        } else {
                            f64::from(rgba[((yy as u32 * w + xx as u32) * 4) as usize + ch])
                        }
                    };
                    let v = (at(x0, y0) * (1.0 - fx) + at(x0 + 1.0, y0) * fx) * (1.0 - fy)
                        + (at(x0, y0 + 1.0) * (1.0 - fx) + at(x0 + 1.0, y0 + 1.0) * fx) * fy;
                    out[((j * ow + i) * 4) as usize + ch] = v.round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        Turned { width: ow, height: oh, rgba: out, angle, centers }
    }

    /// Turned-image pixel → source pixel.
    fn to_source(&self, x: f64, y: f64) -> (f64, f64) {
        let (c, s) = cos_sin(self.angle);
        let (dx, dy) = (x - self.centers[2], y - self.centers[3]);
        (c * dx - s * dy + self.centers[0], s * dx + c * dy + self.centers[1])
    }
}

/// The reading-order similarity of two word sequences: their longest common subsequence over
/// the ground truth's length.
fn order_score(found: &[String], truth: &[String]) -> f64 {
    let mut prev = vec![0usize; truth.len() + 1];
    for f in found {
        let mut cur = vec![0usize; truth.len() + 1];
        for (j, t) in truth.iter().enumerate() {
            cur[j + 1] = if f == t { prev[j] + 1 } else { prev[j + 1].max(cur[j]) };
        }
        prev = cur;
    }
    prev[truth.len()] as f64 / truth.len() as f64
}

struct Measured {
    exact_words: usize,
    order: f64,
    located: usize,
    misplaced: Vec<String>,
    missing: Vec<String>,
    orientation: Value,
    seconds: f64,
}

/// Recognize one fixture through the tools and measure it against the ground truth.
fn run_case(auto: &mut Automation, path: &std::path::Path, name: &str, turned: &Turned, truth: &[Truth], options: Value) -> Measured {
    let png = export::encode_png(turned.width, turned.height, &turned.rgba).unwrap();
    let pdf = Session::new().create_from_images(&[(format!("{name}.png"), png)]).unwrap();
    std::fs::write(path.join(format!("{name}.pdf")), pdf.as_slice()).unwrap();
    let id = result(auto, "doc_open", json!({"path": format!("{name}.pdf")}))["doc"].as_u64().unwrap();
    let image_before =
        PageRenderer::new(pdf.clone(), RenderConfig::default()).render(RenderRequest { scale: 1.0, ..Default::default() }).rgba.to_vec();
    let mut args = json!({"doc": id});
    for (k, v) in options.as_object().unwrap() {
        args[k] = v.clone();
    }
    let started = std::time::Instant::now();
    let reply = result(auto, "ocr_recognize", args);
    let seconds = started.elapsed().as_secs_f64();
    let page = &reply["pages"][0];
    let found: Vec<String> = page["text"].as_str().unwrap_or("").split_whitespace().map(normalize).filter(|w| !w.is_empty()).collect();
    let truth_words: Vec<String> = truth.iter().map(|t| normalize(&t.text)).collect();
    let mut remaining = found.clone();
    let missing: Vec<String> = truth_words
        .iter()
        .filter(|t| {
            let hit = remaining.iter().position(|f| f == *t);
            hit.map(|i| remaining.swap_remove(i)).is_none()
        })
        .cloned()
        .collect();
    let exact_words = truth_words.len() - missing.len();
    // Where the searchable text is: search the saved layer for each true word and require a
    // match whose center maps back inside that word's box (in the source page) — i.e. over the
    // visible word in the unchanged scan image.
    let saved = result(auto, "doc_save", json!({"doc": id, "path": format!("{name}-ocr.pdf")}));
    assert!(saved.is_object());
    let bytes = Arc::new(std::fs::read(path.join(format!("{name}-ocr.pdf"))).unwrap());
    let mut renderer = PageRenderer::new(bytes.clone(), RenderConfig::default());
    let image_after = renderer.render(RenderRequest { scale: 1.0, ..Default::default() }).rgba.to_vec();
    assert!(image_before == image_after, "{name}: the page image must not change");
    let text = renderer.render(RenderRequest { kind: RequestKind::Text, scale: 1.0, ..Default::default() }).text.unwrap_or_default();
    let mut located = 0;
    let mut misplaced = Vec::new();
    for t in truth {
        let needle = normalize(&t.text);
        if !found.contains(&needle) {
            continue;
        }
        let height = t.rect[3] - t.rect[1];
        let on_word = text.find(&needle).iter().any(|range| {
            let glyphs = &text.glyphs[range.clone()];
            let (x0, y0, x1, y1) = glyphs.iter().fold((f64::MAX, f64::MAX, f64::MIN, f64::MIN), |(a, b, c, d), g| {
                (a.min(f64::from(g.rect[0])), b.min(f64::from(g.rect[1])), c.max(f64::from(g.rect[2])), d.max(f64::from(g.rect[3])))
            });
            // One pixel per point: view-space points are turned-image pixels.
            let (sx, sy) = turned.to_source((x0 + x1) / 2.0, (y0 + y1) / 2.0);
            let tol = 0.35 * height;
            sx >= t.rect[0] - tol && sx <= t.rect[2] + tol && sy >= t.rect[1] - tol && sy <= t.rect[3] + tol
        });
        if on_word {
            located += 1;
        } else {
            misplaced.push(t.text.clone());
        }
    }
    let _ = result(auto, "doc_close", json!({"doc": id, "discard_changes": true}));
    Measured { exact_words, order: order_score(&found, &truth_words), located, misplaced, missing, orientation: page["orientation"].clone(), seconds }
}

#[test]
fn ocr_orientation_options_default_off_and_reject_bad_values() {
    let path = root("args");
    let mut auto = Automation::new().with_root(&path).unwrap();
    let doc = result(&mut auto, "doc_create", json!({"from":"blank"}))["doc"].as_u64().unwrap();
    for args in [json!({"doc":doc,"auto_rotate":"yes"}), json!({"doc":doc,"deskew":1})] {
        assert!(auto.call("ocr_recognize", &args).is_err(), "{args}");
        assert!(auto.call("ocr_prepare", &args).is_err(), "{args}");
    }
    let defaults = pdfcraft_engine::ocr::OcrSettings::default();
    assert!(!defaults.auto_rotate && !defaults.deskew, "orientation correction is opt-in");
    let tools = pdfcraft_automation::tools();
    for name in ["ocr_recognize", "ocr_prepare", "ocr_recognize_files"] {
        let tool = tools.iter().find(|t| t.name == name).unwrap();
        let props = &tool.input_schema["properties"];
        assert_eq!(props["auto_rotate"]["type"], "boolean", "{name}");
        assert_eq!(props["deskew"]["type"], "boolean", "{name}");
    }
}

#[test]
fn ocr_auto_rotate_and_deskew_read_turned_and_skewed_scans_onto_the_visible_words() {
    if !pdfcraft_engine::ocr::Models::required_for_tests().unwrap() {
        eprintln!("skipped: OCR models not installed");
        return;
    }
    eprintln!("running actual pinned OCR models on turned and skewed synthetic scans");
    let path = root("models");
    let mut auto = Automation::new().with_root(&path).unwrap();
    let (w, h, rgba, truth) = source();
    let total = truth.len();
    let on = json!({"auto_rotate": true, "deskew": true});
    let mut failures = Vec::new();
    // The required quarter turns and ±3/±7° skews, then turns combined with skew.
    for angle in [0.0, 90.0, 180.0, 270.0, 3.0, -3.0, 7.0, -7.0, 93.0, 173.0, 263.0] {
        let turned = Turned::new(w, h, &rgba, angle);
        let name = format!("turned{}", angle as i32);
        let base = run_case(&mut auto, &path, &format!("{name}-plain"), &turned, &truth, json!({}));
        let m = run_case(&mut auto, &path, &name, &turned, &truth, on.clone());
        eprintln!(
            "angle {angle:>4}: corrected {} in {:.1}s | words exact {}/{total}{}, reading order {:.2}, located on visible word {}/{total}{} | without correction ({:.1}s): exact {}/{total}, order {:.2}, located {}/{total}",
            m.orientation,
            m.seconds,
            m.exact_words,
            if m.missing.is_empty() { String::new() } else { format!(" (missing {:?})", m.missing) },
            m.order,
            m.located,
            if m.misplaced.is_empty() { String::new() } else { format!(" (misplaced {:?})", m.misplaced) },
            base.seconds,
            base.exact_words,
            base.order,
            base.located,
        );
        let quarter = (angle / 90.0_f64).round();
        let expected_rotation = (quarter as i64).rem_euclid(4) as u64 * 90;
        let skew = m.orientation["skew"].as_f64().unwrap_or(f64::NAN);
        let expected_skew = angle - quarter * 90.0;
        if m.orientation["rotation"].as_u64() != Some(expected_rotation) || (skew - expected_skew).abs() > 0.5 {
            failures.push(format!("{angle}: detected {}", m.orientation));
        }
        // Thresholds sit below what was measured (see the report printed above), so a real
        // regression in detection, recognition or placement fails.
        if m.exact_words * 10 < total * 9 || m.order < 0.9 || m.located * 10 < m.exact_words.max(1) * 9 || !m.misplaced.is_empty() {
            failures
                .push(format!("{angle}: exact {}/{total}, order {:.2}, located {}, misplaced {:?}", m.exact_words, m.order, m.located, m.misplaced));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
