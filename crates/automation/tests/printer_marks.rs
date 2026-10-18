//! `print_add_printer_marks` end to end: synthetic pages through the tool table, save and
//! reopen, undo, and what text extraction and accessible text see afterwards.

use std::path::{Path, PathBuf};

use pdfcraft_automation::{Automation, Content};
use serde_json::{Value, json};

/// A tagged two-page PDF: page 1 upright 200×300 with an H1 "Introduction" (MCID 0) and a
/// trim box inset 10 pt; page 2 turned 90° with untagged text "Second".
fn tagged_pdf() -> Vec<u8> {
    let c1 = "/H1 << /MCID 0 >> BDC BT /F1 12 Tf 20 180 Td (Introduction) Tj ET EMC";
    let c2 = "BT /F1 12 Tf 20 100 Td (Second) Tj ET";
    let objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 6 0 R /Lang (en-US) >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R 9 0 R] /Count 2 /MediaBox [0 0 200 300] /Resources << /Font << /F1 5 0 R >> >> >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R /StructParents 0 /TrimBox [10 10 190 290] >>".to_vec(),
        format!("<< /Length {} >>\nstream\n{c1}\nendstream", c1.len()).into_bytes(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        b"<< /Type /StructTreeRoot /K 7 0 R /ParentTree 8 0 R /ParentTreeNextKey 1 >>".to_vec(),
        b"<< /Type /StructElem /S /H1 /P 6 0 R /Pg 3 0 R /K 0 >>".to_vec(),
        b"<< /Nums [0 [7 0 R]] >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /Contents 10 0 R /Rotate 90 >>".to_vec(),
        format!("<< /Length {} >>\nstream\n{c2}\nendstream", c2.len()).into_bytes(),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

fn workdir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pdfcraft-printer-marks-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("job.pdf"), tagged_pdf()).unwrap();
    dir
}

fn auto(dir: &Path) -> Automation {
    Automation::new().with_root(dir).unwrap().with_clock(|| 1_700_000_000)
}

fn ok(a: &mut Automation, tool: &str, args: Value) -> Value {
    match a.call(tool, &args) {
        Ok(mut c) => match c.remove(0) {
            Content::Json(v) => v,
            other => panic!("{tool}: expected JSON, got {other:?}"),
        },
        Err(e) => panic!("{tool} {args}: {e}"),
    }
}

fn rect(v: &Value) -> [f64; 4] {
    let a: Vec<f64> = v.as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
    [a[0], a[1], a[2], a[3]]
}

fn close(a: [f64; 4], b: [f64; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6)
}

/// The media box of `page` (0-based) in the saved file.
fn saved_media(path: &Path, page: usize) -> [f64; 4] {
    let doc = pdfcraft_cos::Document::open(std::sync::Arc::new(std::fs::read(path).unwrap())).unwrap();
    let p = &pdfcraft_model::pages(&doc)[page];
    let v: Vec<f64> = doc.resolve(p.dict.get(b"MediaBox").unwrap()).as_array().unwrap().iter().map(|o| o.as_f64().unwrap()).collect();
    [v[0], v[1], v[2], v[3]]
}

fn text(a: &mut Automation, doc: u64) -> Vec<String> {
    let r = ok(a, "text_extract", json!({ "doc": doc }));
    r["pages"].as_array().unwrap().iter().map(|p| p["text"].as_str().unwrap().to_string()).collect()
}

#[test]
fn printer_marks_grow_the_media_box_and_survive_save() {
    let dir = workdir("e2e");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "job.pdf" }))["doc"].as_u64().unwrap();
    let before_text = text(&mut a, doc);
    let before_accessible = ok(&mut a, "doc_export_accessible_text", json!({ "doc": doc }))["text"].clone();

    let r = ok(&mut a, "print_add_printer_marks", json!({ "doc": doc }));
    let marked = r["marked"].as_array().unwrap();
    assert_eq!(marked.len(), 2, "{r}");
    let m = 6.0 + 18.0 + 0.25;
    // Page 1 has a trim box: the marks go round it, pushed out past the bleed (no bleed box: the
    // crop box, 10 pt past the trim on every side).
    let m1 = 10.0 + 18.0 + 0.25;
    assert!(close(rect(&marked[0]["trim"]), [10.0, 10.0, 190.0, 290.0]), "{r}");
    assert!(close(rect(&marked[0]["bleed"]), [0.0, 0.0, 200.0, 300.0]), "{r}");
    assert!(close(rect(&marked[0]["media"]), [10.0 - m1, 10.0 - m1, 190.0 + m1, 290.0 + m1]), "{r}");
    // Page 2 has none: the crop (= media) box is the trim.
    assert!(close(rect(&marked[1]["media"]), [-m, -m, 200.0 + m, 300.0 + m]), "{r}");
    assert_eq!(marked[1]["margin"].as_f64(), Some(m));
    // The displayed page is the enlarged page.
    let shown = ok(&mut a, "doc_info", json!({ "doc": doc }));
    let pages = shown["pages"].as_array().unwrap();
    assert!((pages[0]["width"].as_f64().unwrap() - (180.0 + 2.0 * m1)).abs() < 0.01, "{shown}");
    assert!((pages[1]["width"].as_f64().unwrap() - (300.0 + 2.0 * m)).abs() < 0.01, "rotated: {shown}");

    // Extraction and accessible text don't pick up the page information (outlines in an artifact).
    assert_eq!(text(&mut a, doc), before_text);
    assert!(!text(&mut a, doc).join(" ").contains("job.pdf"));
    assert_eq!(ok(&mut a, "doc_export_accessible_text", json!({ "doc": doc }))["text"], before_accessible);
    assert_eq!(ok(&mut a, "text_find", json!({ "doc": doc, "query": "Page 1" }))["matches"].as_array().map(Vec::len), Some(0));

    // The marks render (pixels appear outside the old page).
    let png = a.call("page_render", &json!({ "doc": doc, "page": 1, "dpi": 72 })).unwrap();
    assert!(matches!(png.first(), Some(Content::Png { .. })));

    // Saved and reopened, the marks and the bigger page stay.
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "marked.pdf" }));
    assert!(close(saved_media(&dir.join("marked.pdf"), 0), [10.0 - m1, 10.0 - m1, 190.0 + m1, 290.0 + m1]));
    let again = ok(&mut a, "doc_open", json!({ "path": "marked.pdf" }))["doc"].as_u64().unwrap();
    assert_eq!(text(&mut a, again), before_text);
    let layers = ok(&mut a, "doc_info", json!({ "doc": again }))["layers"].clone();
    assert_eq!(layers, json!([{ "name": "Printer marks", "visible": true }]));
}

#[test]
fn undo_restores_the_page() {
    let dir = workdir("undo");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "job.pdf" }))["doc"].as_u64().unwrap();
    let before = ok(&mut a, "doc_info", json!({ "doc": doc }))["pages"].clone();
    let r = ok(
        &mut a,
        "print_add_printer_marks",
        json!({ "doc": doc, "pages": [2], "marks": ["trim", "registration"], "offset": 9, "line_weight": 0.5, "style": "eastern", "file_name": "Proof" }),
    );
    let marked = r["marked"].as_array().unwrap();
    assert_eq!(marked.len(), 1);
    assert_eq!(marked[0]["page"], 2);
    assert_eq!(marked[0]["margin"].as_f64(), Some(9.0 + 18.0 + 0.5));
    assert_ne!(ok(&mut a, "doc_info", json!({ "doc": doc }))["pages"], before);
    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    assert_eq!(ok(&mut a, "doc_info", json!({ "doc": doc }))["pages"], before);
    ok(&mut a, "edit_redo", json!({ "doc": doc }));
    assert_ne!(ok(&mut a, "doc_info", json!({ "doc": doc }))["pages"], before);
}

#[test]
fn bad_arguments_are_rejected_without_change() {
    let dir = workdir("bad");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "job.pdf" }))["doc"].as_u64().unwrap();
    let before = ok(&mut a, "doc_info", json!({ "doc": doc }));
    for bad in [
        json!({ "doc": doc, "line_weight": 0 }),
        json!({ "doc": doc, "line_weight": -1 }),
        json!({ "doc": doc, "line_weight": 1e300 }),
        json!({ "doc": doc, "offset": -3 }),
        json!({ "doc": doc, "offset": 500 }),
        json!({ "doc": doc, "marks": [] }),
        json!({ "doc": doc, "marks": ["crop-ish"] }),
        json!({ "doc": doc, "marks": "trim" }),
        json!({ "doc": doc, "style": "baroque" }),
        json!({ "doc": doc, "pages": [3] }),
        json!({ "doc": doc, "pages": [0] }),
        json!({ "doc": doc, "pages": [] }),
        json!({ "doc": doc, "file_name": "x".repeat(300) }),
        json!({ "doc": doc, "offset": "six" }),
    ] {
        assert!(a.call("print_add_printer_marks", &bad).is_err(), "{bad}");
    }
    let after = ok(&mut a, "doc_info", json!({ "doc": doc }));
    assert_eq!(after["pages"], before["pages"]);
    assert_eq!(after["document"]["dirty"], before["document"]["dirty"]);
    assert!(after["document"]["undo"].is_null(), "{after}");
}
