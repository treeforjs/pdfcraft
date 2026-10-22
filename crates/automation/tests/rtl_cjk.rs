//! End-to-end tests of editing right-to-left and CJK text (`text_paragraphs`, `text_direction`,
//! `text_edit`) on synthetic PDFs: logical order, display order in the saved content, caret
//! movement, vertical writing, undo/redo, save/reopen and argument errors.

use std::path::{Path, PathBuf};

use pdfcraft_automation::{Automation, Content};
use serde_json::{Value, json};

fn pdf(objs: &[String]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

/// A page showing `shown` (display order) in a Type 0 font with `encoding` whose codes
/// 0x0101… are `chars`, with a ToUnicode map.
fn font_pdf(encoding: &str, chars: &[char], shown: &str, at: (u32, u32)) -> Vec<u8> {
    let code = |c: char| 0x0101 + chars.iter().position(|x| *x == c).unwrap();
    let hex: String = shown.chars().map(|c| format!("{:04X}", code(c))).collect();
    let content = format!("BT /F1 12 Tf {} {} Td <{hex}> Tj ET", at.0, at.1);
    let mut bf = String::new();
    for (i, c) in chars.iter().enumerate() {
        let u: String = c.encode_utf16(&mut [0; 2]).iter().map(|u| format!("{u:04X}")).collect();
        bf.push_str(&format!("<{:04X}> <{u}>\n", 0x0101 + i));
    }
    let cmap = format!(
        "/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CMapType 2 def 1 begincodespacerange <0000> <FFFF> endcodespacerange {} beginbfchar\n{bf}endbfchar endcmap CMapName currentdict /CMap defineresource pop end end",
        chars.len()
    );
    pdf(&[
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 600 800] >>".into(),
        "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".into(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        format!("<< /Type /Font /Subtype /Type0 /BaseFont /TestFace /Encoding /{encoding} /DescendantFonts [6 0 R] /ToUnicode 7 0 R >>"),
        "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /TestFace /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /DW 1000 >>".into(),
        format!("<< /Length {} >>\nstream\n{cmap}\nendstream", cmap.len()),
    ])
}

fn hebrew() -> Vec<char> {
    let mut chars: Vec<char> = ('\u{05D0}'..='\u{05EA}').collect();
    chars.extend(" 0123456789".chars());
    chars
}

fn workdir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pdfcraft-rtl-cjk-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("hebrew.pdf"), font_pdf("Identity-H", &hebrew(), "םולש", (300, 700))).unwrap();
    let japanese: Vec<char> = "日本語縦書きの文字列".chars().collect();
    std::fs::write(dir.join("vertical.pdf"), font_pdf("Identity-V", &japanese, "日本語", (300, 700))).unwrap();
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

fn err(a: &mut Automation, tool: &str, args: Value) -> String {
    match a.call(tool, &args) {
        Ok(c) => panic!("{tool} {args}: expected an error, got {c:?}"),
        Err(e) => e.to_string(),
    }
}

fn paragraph(a: &mut Automation, doc: u64) -> Value {
    ok(a, "text_paragraphs", json!({ "doc": doc, "page": 1 }))["paragraphs"][0].clone()
}

#[test]
fn hebrew_paragraphs_are_edited_in_logical_order() {
    let dir = workdir("hebrew");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "hebrew.pdf" }))["doc"].as_u64().unwrap();
    let p = paragraph(&mut a, doc);
    assert_eq!((p["text"].as_str(), p["direction"].as_str(), p["vertical"].as_bool()), (Some("שלום"), Some("rtl"), Some(false)), "{p}");
    let right = p["rect"][2].as_f64().unwrap();

    let d = ok(&mut a, "text_direction", json!({ "doc": doc, "page": 1, "paragraph": 1 }));
    assert_eq!((d["direction"].as_str(), d["display"].as_str()), (Some("rtl"), Some("םולש")), "{d}");
    assert_eq!(d["display_order"], json!([3, 2, 1, 0]));
    assert_eq!(d["scripts"], json!(["hebrew"]));
    // The right arrow moves towards the start of a right-to-left word.
    let c = ok(&mut a, "text_direction", json!({ "doc": doc, "page": 1, "paragraph": 1, "caret": 4, "move": "right", "steps": 2 }));
    assert_eq!(c["caret_path"], json!([3, 2]), "{c}");

    ok(&mut a, "text_edit", json!({ "doc": doc, "page": 1, "paragraph": 1, "text": "שלום עולם 2026" }));
    let p = paragraph(&mut a, doc);
    assert_eq!(p["text"], "שלום עולם 2026");
    assert!((p["rect"][2].as_f64().unwrap() - right).abs() < 0.5, "keeps its right edge: {p}");
    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    assert_eq!(paragraph(&mut a, doc)["text"], "שלום");
    ok(&mut a, "edit_redo", json!({ "doc": doc }));
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "edited.pdf" }));
    let again = ok(&mut a, "doc_open", json!({ "path": "edited.pdf" }))["doc"].as_u64().unwrap();
    assert_eq!(paragraph(&mut a, again)["text"], "שלום עולם 2026");
    // The saved page shows it in display order: the number first, then the words reversed.
    let d = ok(&mut a, "text_direction", json!({ "doc": again, "page": 1, "paragraph": 1 }));
    assert_eq!(d["display"], "2026 םלוע םולש");
}

#[test]
fn vertical_columns_take_new_text_in_their_own_font_only() {
    let dir = workdir("vertical");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "vertical.pdf" }))["doc"].as_u64().unwrap();
    let p = paragraph(&mut a, doc);
    assert_eq!((p["text"].as_str(), p["vertical"].as_bool(), p["direction"].as_str()), (Some("日本語"), Some(true), Some("ltr")), "{p}");
    // A column: taller than wide.
    let r: Vec<f64> = p["rect"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    assert!(r[3] - r[1] > 2.5 * (r[2] - r[0]), "{r:?}");
    ok(&mut a, "text_edit", json!({ "doc": doc, "page": 1, "paragraph": 1, "text": "縦書きの文字列" }));
    assert_eq!(paragraph(&mut a, doc)["text"], "縦書きの文字列");
    let d = ok(&mut a, "text_direction", json!({ "doc": doc, "page": 1, "paragraph": 1 }));
    assert_eq!((d["vertical"].as_bool(), d["scripts"].clone()), (Some(true), json!(["han", "kana"])));
    let e = err(&mut a, "text_edit", json!({ "doc": doc, "page": 1, "paragraph": 1, "size": 20 }));
    assert!(e.contains("vertical"), "{e}");
    let e = err(&mut a, "text_edit", json!({ "doc": doc, "page": 1, "paragraph": 1, "text": "漢字" }));
    assert!(e.contains("vertical"), "{e}");
    assert_eq!(paragraph(&mut a, doc)["text"], "縦書きの文字列", "refused edits change nothing");
}

#[test]
fn text_direction_checks_its_arguments() {
    let dir = workdir("args");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "hebrew.pdf" }))["doc"].as_u64().unwrap();
    let d = ok(&mut a, "text_direction", json!({ "text": "abc \u{202E}שלום\u{202C} 12" }));
    assert_eq!(d["direction"], "ltr");
    assert!(!d["display"].as_str().unwrap().contains('\u{202E}'));
    let mixed = ok(&mut a, "text_direction", json!({ "text": "שלום abc", "caret": 0, "move": "left", "steps": 10000 }));
    assert!(mixed["caret"].as_u64().unwrap() <= 8);
    for (args, needle) in [
        (json!({}), "pass text"),
        (json!({ "text": "a", "doc": doc, "page": 1, "paragraph": 1 }), "either"),
        (json!({ "doc": doc, "page": 1, "paragraph": 9 }), "out of range"),
        (json!({ "doc": doc, "page": 1, "paragraph": 0 }), ""),
        (json!({ "text": "abc", "caret": 4, "move": "left" }), "caret"),
        (json!({ "text": "abc", "caret": -1, "move": "left" }), ""),
        (json!({ "text": "abc", "caret": 1 }), "move"),
        (json!({ "text": "abc", "caret": 1, "move": "up" }), ""),
        (json!({ "text": "abc", "caret": 1, "move": "left", "steps": 0 }), ""),
        (json!({ "text": "abc", "direction": "sideways" }), ""),
    ] {
        let e = err(&mut a, "text_direction", args.clone());
        assert!(e.contains(needle), "{args}: {e}");
    }
    // Long text: no display order list, still a direction and caret.
    let long = "א".repeat(20_000);
    let d = ok(&mut a, "text_direction", json!({ "text": long, "caret": 0, "move": "left" }));
    assert!(d.get("display_order").is_none() && d["caret"] == 1);
}
