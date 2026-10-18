//! Export ▸ Run OCR (parity `create.export-ocr`) end to end through the export tools, on an
//! original synthetic scan: a PdfCraft-made text page rendered at 200 dpi (11 pt Helvetica, an
//! x-height of about 16 px), cropped to its text and wrapped as an image-only PDF.
//!
//! With OCR (the default auto, or on) the exported Word, HTML, RTF, Excel, PowerPoint and text
//! files hold the page's words; with it off they hold none of them. The document itself never
//! changes. Assertions are recall-style (most of the words found), not exact strings.
//! Model-backed tests skip without the models; set PDFCRAFT_MODELS_REQUIRED=1 so missing models
//! fail instead.
use pdfcraft_automation::{Automation, Content};
use pdfcraft_engine::{Session, export};
use pdfcraft_render::{PageRenderer, RenderConfig, RenderRequest};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const TEXT: &str = "Quarterly harvest figures exceeded every forecast.\nMarigold orchards shipped seventeen crates northward.\nInspectors approved the granary before winter.";
const DPI: f64 = 200.0;

fn result(auto: &mut Automation, name: &str, args: Value) -> Value {
    let mut reply = auto.call(name, &args).unwrap_or_else(|e| panic!("{name}: {e}"));
    match reply.remove(0) {
        Content::Json(value) => value,
        other => panic!("expected JSON, got {other:?}"),
    }
}

fn root(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("pdfcraft-export-ocr-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn normalize(word: &str) -> String {
    word.chars().filter(char::is_ascii_alphanumeric).flat_map(char::to_lowercase).collect()
}

fn truth() -> Vec<String> {
    TEXT.split_whitespace().map(normalize).filter(|w| !w.is_empty()).collect()
}

/// An image-only PDF of TEXT: the text page rendered at `DPI`, cropped to its ink with a
/// margin, one pixel per point.
fn scan_pdf() -> std::sync::Arc<Vec<u8>> {
    let session = Session::new();
    let bytes = session.create_from_text("Export OCR fixture", TEXT).unwrap();
    let shot = PageRenderer::new(bytes, RenderConfig::default()).render(RenderRequest { scale: (DPI / 72.0) as f32, ..Default::default() });
    assert!(shot.error.is_none(), "{:?}", shot.error);
    let (w, h) = (shot.width as usize, shot.height as usize);
    // Flatten onto white and find the ink.
    let mut rgb = vec![255u8; w * h * 4];
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            let p = &shot.rgba[i..i + 4];
            let over = |c: u8| (u32::from(c) + 255 - u32::from(p[3])).min(255) as u8;
            let px = [over(p[0]), over(p[1]), over(p[2]), 255];
            rgb[i..i + 4].copy_from_slice(&px);
            if px[0] < 128 {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            }
        }
    }
    assert!(x1 > x0 && y1 > y0, "the fixture page has ink");
    let m = 60;
    let (cx0, cy0, cx1, cy1) = (x0.saturating_sub(m), y0.saturating_sub(m), (x1 + m).min(w - 1), (y1 + m).min(h - 1));
    let (cw, ch) = (cx1 - cx0 + 1, cy1 - cy0 + 1);
    let mut crop = Vec::with_capacity(cw * ch * 4);
    for y in cy0..=cy1 {
        crop.extend_from_slice(&rgb[(y * w + cx0) * 4..(y * w + cx1 + 1) * 4]);
    }
    let png = export::encode_png(cw as u32, ch as u32, &crop).unwrap();
    Session::new().create_from_images(&[("scan.png".to_string(), png)]).unwrap()
}

/// The readable text of an exported file: XML text nodes of the Office packages' parts, HTML
/// without its tags, RTF without its control words, plain text as it is.
fn exported_text(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap();
    match path.extension().and_then(|e| e.to_str()).unwrap() {
        "docx" | "xlsx" | "pptx" => unzip(&bytes)
            .into_iter()
            .filter(|(name, _)| name.ends_with(".xml") && !name.starts_with("docProps/"))
            .map(|(name, xml)| {
                let doc = roxmltree::Document::parse(&xml).unwrap_or_else(|e| panic!("{name}: {e}"));
                doc.descendants().filter(|n| n.is_text()).filter_map(|n| n.text()).collect::<Vec<_>>().join(" ")
            })
            .collect::<Vec<_>>()
            .join(" "),
        "html" => {
            let html = String::from_utf8(bytes).unwrap();
            let body = html.split_once("<body").map_or(html.as_str(), |(_, b)| b);
            let mut out = String::new();
            let mut in_tag = false;
            for c in body.chars() {
                match c {
                    '<' => in_tag = true,
                    '>' => {
                        in_tag = false;
                        out.push(' ');
                    }
                    _ if !in_tag => out.push(c),
                    _ => {}
                }
            }
            out
        }
        "rtf" => {
            let rtf = String::from_utf8(bytes).unwrap();
            let mut out = String::new();
            let mut chars = rtf.chars().peekable();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => {
                        while chars.peek().is_some_and(|c| c.is_ascii_alphanumeric() || *c == '-') {
                            chars.next();
                        }
                        out.push(' ');
                    }
                    '{' | '}' => out.push(' '),
                    _ => out.push(c),
                }
            }
            out
        }
        _ => String::from_utf8(bytes).unwrap(),
    }
}

/// The inflated parts of a zip archive written with local headers that carry sizes.
fn unzip(zip: &[u8]) -> Vec<(String, String)> {
    let at = |i: usize, n: usize| zip[i..i + n].iter().rev().fold(0usize, |v, b| v << 8 | *b as usize);
    let mut parts = Vec::new();
    let mut i = 0;
    while zip[i..].starts_with(b"PK\x03\x04") {
        let (method, size, name_len, extra) = (at(i + 8, 2), at(i + 18, 4), at(i + 26, 2), at(i + 28, 2));
        let name = String::from_utf8(zip[i + 30..i + 30 + name_len].to_vec()).unwrap();
        let data = i + 30 + name_len + extra;
        let raw = &zip[data..data + size];
        let mut text = String::new();
        if method == 8 {
            std::io::Read::read_to_string(&mut flate2::read::DeflateDecoder::new(raw), &mut text).unwrap();
        } else {
            text = String::from_utf8_lossy(raw).into_owned();
        }
        parts.push((name, text));
        i = data + size;
    }
    parts
}

/// How many of the fixture's words the text holds (each true word matched at most once).
fn recalled(text: &str) -> usize {
    let mut found: Vec<String> = text.split_whitespace().map(normalize).filter(|w| !w.is_empty()).collect();
    truth()
        .iter()
        .filter(|t| {
            let hit = found.iter().position(|f| f == *t);
            hit.map(|i| found.swap_remove(i)).is_some()
        })
        .count()
}

/// One export tool call for `file` (its extension picks the tool), with extra arguments.
fn export_args(doc: u64, file: &str, extra: Value) -> (&'static str, Value) {
    let tool = match file.rsplit_once('.').map(|(_, e)| e) {
        Some("pptx") => "doc_export_pptx",
        Some("txt") => "doc_export_text",
        _ => "doc_export_office",
    };
    let mut args = json!({ "doc": doc, "path": file });
    for (k, v) in extra.as_object().unwrap() {
        args[k] = v.clone();
    }
    (tool, args)
}

fn open_scan(auto: &mut Automation, dir: &Path) -> (u64, Vec<u8>) {
    let pdf = scan_pdf();
    std::fs::write(dir.join("scan.pdf"), pdf.as_slice()).unwrap();
    let doc = result(auto, "doc_open", json!({ "path": "scan.pdf" }))["doc"].as_u64().unwrap();
    (doc, pdf.to_vec())
}

/// The document is as it was opened: not dirty, nothing to undo, and Save writes the original.
fn assert_unchanged(auto: &mut Automation, dir: &Path, doc: u64, original: &[u8]) {
    let list = result(auto, "doc_list", json!({}));
    let d = list["documents"].as_array().unwrap().iter().find(|d| d["doc"] == doc).unwrap().clone();
    assert_eq!(d["dirty"], false, "{d}");
    assert!(d["undo"].is_null(), "{d}");
    result(auto, "doc_save", json!({ "doc": doc, "path": "saved.pdf", "full": false }));
    assert!(std::fs::read(dir.join("saved.pdf")).unwrap() == original, "saving after the exports writes the original file");
}

#[test]
fn export_ocr_arguments_are_in_the_schemas_and_validated() {
    let dir = root("args");
    let mut auto = Automation::new().with_root(&dir).unwrap();
    let doc = result(&mut auto, "doc_create", json!({ "from": "blank" }))["doc"].as_u64().unwrap();
    let tools = pdfcraft_automation::tools();
    for name in ["doc_export_office", "doc_export_pptx", "doc_export_text"] {
        let tool = tools.iter().find(|t| t.name == name).unwrap();
        let props = &tool.input_schema["properties"];
        assert_eq!(props["ocr"]["enum"], json!(["auto", "on", "off"]), "{name}");
        assert_eq!(props["ocr_language"]["enum"], json!(["en"]), "{name}");
    }
    for file in ["a.docx", "a.pptx", "a.txt"] {
        for extra in [json!({ "ocr": "always" }), json!({ "ocr": true }), json!({ "ocr_language": "xx" }), json!({ "ocr_language": 1 })] {
            let (tool, args) = export_args(doc, file, extra);
            assert!(auto.call(tool, &args).is_err(), "{tool} {args}");
        }
    }
    assert!(!dir.join("a.docx").exists() && !dir.join("a.pptx").exists() && !dir.join("a.txt").exists(), "bad arguments write nothing");
    // A blank page has no text and no image: auto has nothing to recognize, with or without
    // models, and says which pages had no text.
    let out = result(&mut auto, "doc_export_text", json!({ "doc": doc, "path": "blank.txt" }));
    assert_eq!(out["ocr"]["mode"], "auto");
    assert_eq!(out["ocr"]["ran"], false);
    assert_eq!(out["ocr"]["pages_without_text"], json!([1]));
    assert_eq!(out["ocr"]["scanned_pages"], json!([]));
    assert!(out["ocr"]["not_run"].is_null(), "{out}");
}

#[test]
fn export_ocr_auto_without_models_exports_and_says_why_and_on_fails() {
    if pdfcraft_engine::ocr::available() {
        eprintln!("skipped: OCR models are installed (this test covers their absence)");
        return;
    }
    let dir = root("nomodels");
    let mut auto = Automation::new().with_root(&dir).unwrap();
    let (doc, original) = open_scan(&mut auto, &dir);
    let out = result(&mut auto, "doc_export_office", json!({ "doc": doc, "path": "scan.docx" }));
    assert_eq!(out["ocr"]["ran"], false);
    assert_eq!(out["ocr"]["scanned_pages"], json!([1]));
    assert!(out["ocr"]["not_run"].as_str().unwrap().contains("no OCR models"), "{out}");
    assert!(dir.join("scan.docx").exists());
    let err = auto.call("doc_export_text", &json!({ "doc": doc, "path": "scan.txt", "ocr": "on" })).unwrap_err().to_string();
    assert!(err.contains("no OCR models"), "{err}");
    assert!(!dir.join("scan.txt").exists(), "a failed export writes nothing");
    assert_unchanged(&mut auto, &dir, doc, &original);
}

#[test]
fn export_ocr_puts_recognized_text_in_every_text_format_and_off_leaves_it_out() {
    if !pdfcraft_engine::ocr::Models::required_for_tests().unwrap() {
        eprintln!("skipped: OCR models not installed");
        return;
    }
    eprintln!("running actual pinned OCR models on a synthetic scan for every export format");
    let dir = root("models");
    let mut auto = Automation::new().with_root(&dir).unwrap();
    let (doc, original) = open_scan(&mut auto, &dir);
    let total = truth().len();
    let need = (total * 4).div_ceil(5);
    let mut failures = Vec::new();
    for ext in ["docx", "html", "rtf", "xlsx", "pptx", "txt"] {
        // Default (auto), on, and off.
        for (label, extra) in [("auto", json!({})), ("on", json!({ "ocr": "on", "ocr_language": "en" })), ("off", json!({ "ocr": "off" }))] {
            let file = format!("scan-{label}.{ext}");
            let (tool, args) = export_args(doc, &file, extra);
            let started = std::time::Instant::now();
            let out = result(&mut auto, tool, args);
            let text = exported_text(&dir.join(&file));
            let found = recalled(&text);
            eprintln!("{ext:>4} ocr {label:<4}: {found}/{total} words in {:.1}s, ocr {}", started.elapsed().as_secs_f64(), out["ocr"]);
            assert_eq!(out["ocr"]["mode"], label, "{ext}: {out}");
            if label == "off" {
                assert_eq!(out["ocr"]["ran"], false, "{ext}: {out}");
                if found != 0 {
                    failures.push(format!("{ext} off: {found} words appeared without OCR: {text}"));
                }
            } else {
                assert_eq!(out["ocr"]["ran"], true, "{ext} {label}: {out}");
                assert_eq!(out["ocr"]["recognized_pages"], json!([1]), "{ext} {label}: {out}");
                assert_eq!(out["ocr"]["scanned_pages"], json!([1]), "{ext} {label}: {out}");
                assert!(out["ocr"]["words"].as_u64().unwrap() >= need as u64, "{ext} {label}: {out}");
                if found < need {
                    failures.push(format!("{ext} {label}: only {found}/{total} words: {text}"));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
    // PowerPoint image mode exports pictures only: nothing is recognized.
    let out = result(&mut auto, "doc_export_pptx", json!({ "doc": doc, "path": "pictures.pptx", "mode": "image" }));
    assert_eq!(out["ocr"]["ran"], false, "{out}");
    // Recognition went into temporary copies: the document has no text and no history.
    let out = result(&mut auto, "doc_export_text", json!({ "doc": doc, "path": "after.txt", "ocr": "off" }));
    assert_eq!(out["ocr"]["pages_without_text"], json!([]), "off does not look at the pages: {out}");
    assert_eq!(recalled(&std::fs::read_to_string(dir.join("after.txt")).unwrap()), 0);
    assert_unchanged(&mut auto, &dir, doc, &original);
}

#[test]
fn export_ocr_auto_recognizes_only_the_scanned_pages() {
    if !pdfcraft_engine::ocr::Models::required_for_tests().unwrap() {
        eprintln!("skipped: OCR models not installed");
        return;
    }
    let dir = root("mixed");
    let mut auto = Automation::new().with_root(&dir).unwrap();
    std::fs::write(dir.join("scan.pdf"), scan_pdf().as_slice()).unwrap();
    let typed = Session::new().create_from_text("Typed", "Lighthouse keepers logged calm seas.").unwrap();
    std::fs::write(dir.join("typed.pdf"), typed.as_slice()).unwrap();
    // Page 1 has a text layer, page 2 is the scan, page 3 is blank.
    let combined = result(&mut auto, "doc_combine", json!({ "paths": ["typed.pdf", "scan.pdf"] }));
    let doc = combined["document"]["doc"].as_u64().unwrap();
    result(&mut auto, "page_insert_blank", json!({ "doc": doc, "at": 3 }));
    let out = result(&mut auto, "doc_export_text", json!({ "doc": doc, "path": "mixed.txt" }));
    assert_eq!(out["ocr"]["pages_without_text"], json!([2, 3]), "{out}");
    assert_eq!(out["ocr"]["scanned_pages"], json!([2]), "{out}");
    assert_eq!(out["ocr"]["recognized_pages"], json!([2]), "{out}");
    let text = std::fs::read_to_string(dir.join("mixed.txt")).unwrap();
    let pages: Vec<&str> = text.split('\u{c}').collect();
    assert_eq!(pages.len(), 3, "{text:?}");
    assert!(pages[0].contains("Lighthouse keepers logged calm seas."), "{text:?}");
    assert!(recalled(pages[1]) >= (truth().len() * 4).div_ceil(5), "{text:?}");
    assert!(pages[2].trim().is_empty(), "{text:?}");
    // Only the exported pages are looked at.
    let out = result(&mut auto, "doc_export_text", json!({ "doc": doc, "path": "first.txt", "pages": [1] }));
    assert_eq!(out["ocr"]["pages_without_text"], json!([]), "{out}");
    assert_eq!(out["ocr"]["ran"], false, "{out}");
}
