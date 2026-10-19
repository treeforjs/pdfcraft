//! End-to-end tests of bulleted and numbered lists (`text_list_set`, `text_list_remove`,
//! `text_list_indent`) on synthetic PDFs: extracted text, hanging-indent positions, tag
//! structure, undo/redo and save/reopen.

use std::path::{Path, PathBuf};

use pdfcraft_automation::{Automation, Content};
use serde_json::{Value, json};

fn pdf(objs: &[Vec<u8>]) -> Vec<u8> {
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

/// One blank 200×300 pt page.
fn blank_pdf() -> Vec<u8> {
    pdf(&[
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 300] >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << >> >>".to_vec(),
        b"<< /Length 0 >>\nstream\n\nendstream".to_vec(),
    ])
}

/// A tagged page with three paragraphs (Document > P, P, P; MCIDs 0–2).
fn tagged_pdf() -> Vec<u8> {
    let c = "/P << /MCID 0 >> BDC BT /F1 12 Tf 20 250 Td (Apples) Tj ET EMC \
             /P << /MCID 1 >> BDC BT /F1 12 Tf 20 210 Td (- Pears) Tj ET EMC \
             /P << /MCID 2 >> BDC BT /F1 12 Tf 20 170 Td (Plums) Tj ET EMC";
    pdf(&[
        b"<< /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 6 0 R /Lang (en-US) >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 300] >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R /StructParents 0 /Resources << /Font << /F1 5 0 R >> >> >>".to_vec(),
        format!("<< /Length {} >>\nstream\n{c}\nendstream", c.len()).into_bytes(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        b"<< /Type /StructTreeRoot /K 7 0 R /ParentTree 11 0 R /ParentTreeNextKey 1 >>".to_vec(),
        b"<< /Type /StructElem /S /Document /P 6 0 R /K [8 0 R 9 0 R 10 0 R] >>".to_vec(),
        b"<< /Type /StructElem /S /P /P 7 0 R /Pg 3 0 R /K 0 >>".to_vec(),
        b"<< /Type /StructElem /S /P /P 7 0 R /Pg 3 0 R /K 1 >>".to_vec(),
        b"<< /Type /StructElem /S /P /P 7 0 R /Pg 3 0 R /K 2 >>".to_vec(),
        b"<< /Nums [0 [8 0 R 9 0 R 10 0 R]] >>".to_vec(),
    ])
}

fn workdir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pdfcraft-lists-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("blank.pdf"), blank_pdf()).unwrap();
    std::fs::write(dir.join("tagged.pdf"), tagged_pdf()).unwrap();
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

fn text(a: &mut Automation, doc: u64) -> String {
    ok(a, "text_extract", json!({ "doc": doc }))["pages"][0]["text"].as_str().unwrap().to_owned()
}

/// (left edge, text) of every text line on page 1, top to bottom.
fn lines(a: &mut Automation, doc: u64) -> Vec<(f64, String)> {
    let out = ok(a, "text_lines", json!({ "doc": doc, "page": 1 }));
    out["lines"].as_array().unwrap().iter().map(|l| (l["rect"][0].as_f64().unwrap(), l["text"].as_str().unwrap().to_owned())).collect()
}

fn labels(a: &mut Automation, doc: u64) -> Vec<Value> {
    let out = ok(a, "content_list", json!({ "doc": doc }));
    out["items"][0]["list"].as_array().cloned().unwrap_or_default().iter().map(|e| e["label"].clone()).collect()
}

fn outline(a: &mut Automation, doc: u64) -> Vec<(u64, String)> {
    let t = ok(a, "tags_tree", json!({ "doc": doc }));
    t["elements"].as_array().unwrap().iter().map(|e| (e["depth"].as_u64().unwrap(), e["standard"].as_str().unwrap().to_owned())).collect()
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 1.0
}

#[test]
fn bulleted_and_numbered_lists_in_a_text_box() {
    let dir = workdir("box");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "blank.pdf" }))["doc"].as_u64().unwrap();
    let body = "Alpha beta gamma delta epsilon zeta eta theta\nSecond item\nThird item";
    ok(&mut a, "page_add_text", json!({ "doc": doc, "page": 1, "at": [20, 20], "width": 120, "size": 10, "text": body }));
    let plain = lines(&mut a, doc);
    assert!(plain.iter().all(|(x, _)| near(*x, 20.0)), "{plain:?}");

    // Bullets: a label before each paragraph, every line of it at the hanging indent.
    let out = ok(&mut a, "text_list_set", json!({ "doc": doc, "page": 1, "block": 1, "style": "disc" }));
    assert_eq!(out["block"], 1);
    assert_eq!(out["list"][0]["label"], "\u{2022}", "{out}");
    let t = text(&mut a, doc);
    assert_eq!(t.matches('\u{2022}').count(), 3, "{t}");
    assert!(t.contains("\u{2022} Alpha") && t.contains("\u{2022} Second item") && t.contains("\u{2022} Third item"), "{t}");
    let l = lines(&mut a, doc);
    assert!(l.len() >= 4, "the first item wraps: {l:?}");
    assert!(l[0].1.starts_with('\u{2022}') && near(l[0].0, 20.0), "{l:?}");
    let body_x = l[1].0;
    assert!(body_x > 30.0 && body_x < 40.0 && !l[1].1.starts_with('\u{2022}'), "hanging indent: {l:?}");
    let second = l.iter().position(|(_, s)| s.contains("Second")).unwrap();
    assert!(near(l[second].0, 20.0), "{l:?}");
    // The paragraph's first word lines up with its wrapped lines.
    let glyphs = ok(&mut a, "text_extract", json!({ "doc": doc, "include_glyphs": true }))["pages"][0]["glyphs"].clone();
    let alpha = glyphs.as_array().unwrap().iter().find(|g| g["text"] == "A").unwrap()["rect"][0].as_f64().unwrap();
    assert!(near(alpha, body_x), "{alpha} vs {body_x}");

    // Numbered, starting at 3; changing the type keeps the indent.
    ok(&mut a, "text_list_set", json!({ "doc": doc, "page": 1, "block": 1, "style": "decimal", "start": 3 }));
    let t = text(&mut a, doc);
    assert!(t.contains("3. Alpha") && t.contains("4. Second") && t.contains("5. Third"), "{t}");
    assert!(near(lines(&mut a, doc)[1].0, body_x));
    ok(&mut a, "text_list_set", json!({ "doc": doc, "page": 1, "block": 1, "style": "upper-roman", "start": 1 }));
    assert_eq!(labels(&mut a, doc), [json!("I."), json!("II."), json!("III.")]);

    // Indent the last two: a nested list numbered on its own, its labels at the parent's body.
    ok(&mut a, "text_list_set", json!({ "doc": doc, "page": 1, "block": 1, "paragraphs": [2, 3], "style": "lower-alpha" }));
    ok(&mut a, "text_list_indent", json!({ "doc": doc, "page": 1, "block": 1, "paragraphs": [2, 3] }));
    assert_eq!(labels(&mut a, doc), [json!("I."), json!("a."), json!("b.")]);
    let l = lines(&mut a, doc);
    let second = l.iter().position(|(_, s)| s.contains("Second")).unwrap();
    assert!(near(l[second].0, body_x), "nested label at the parent's body: {l:?}");
    // Outdent at the top level changes nothing (and adds no undo step).
    ok(&mut a, "text_list_indent", json!({ "doc": doc, "page": 1, "block": 1, "paragraphs": [2, 3], "levels": -1 }));
    assert_eq!(labels(&mut a, doc), [json!("I."), json!("b."), json!("c.")], "back at level 0, counting on");
    ok(&mut a, "text_list_set", json!({ "doc": doc, "page": 1, "block": 1, "style": "upper-roman" }));
    let before = ok(&mut a, "content_list", json!({ "doc": doc }));
    ok(&mut a, "text_list_indent", json!({ "doc": doc, "page": 1, "block": 1, "paragraphs": [1, 1], "levels": -1 }));
    assert_eq!(ok(&mut a, "content_list", json!({ "doc": doc })), before);
    assert_eq!(labels(&mut a, doc), [json!("I."), json!("II."), json!("III.")]);

    // A paragraph typed into the list continues its numbering.
    ok(&mut a, "content_update", json!({ "doc": doc, "page": 1, "index": 1, "text": format!("{body}\nFourth item") }));
    assert_eq!(labels(&mut a, doc), [json!("I."), json!("II."), json!("III."), json!("IV.")]);
    assert!(text(&mut a, doc).contains("IV. Fourth item"));

    // Remove the list from the second paragraph: the list restarts after it.
    ok(&mut a, "text_list_remove", json!({ "doc": doc, "page": 1, "block": 1, "paragraphs": [2, 2] }));
    assert_eq!(labels(&mut a, doc), [json!("I."), Value::Null, json!("I."), json!("II.")]);
    assert!(near(lines(&mut a, doc).iter().find(|(_, s)| s.contains("Second")).unwrap().0, 20.0));

    // Undo and redo, one step each.
    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    assert_eq!(labels(&mut a, doc), [json!("I."), json!("II."), json!("III."), json!("IV.")]);
    ok(&mut a, "edit_redo", json!({ "doc": doc }));
    assert_eq!(labels(&mut a, doc), [json!("I."), Value::Null, json!("I."), json!("II.")]);

    // Through save and reopen.
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "list.pdf" }));
    let again = ok(&mut a, "doc_open", json!({ "path": "list.pdf" }))["doc"].as_u64().unwrap();
    assert_eq!(labels(&mut a, again), [json!("I."), Value::Null, json!("I."), json!("II.")]);
    let t = text(&mut a, again);
    assert!(t.contains("I. Alpha") && t.contains("Second item") && t.contains("II. Fourth item"), "{t}");
    // Remove it all: ordinary paragraphs again.
    ok(&mut a, "text_list_remove", json!({ "doc": again, "page": 1, "block": 1 }));
    assert!(ok(&mut a, "content_list", json!({ "doc": again }))["items"][0].get("list").is_none());
    assert!(lines(&mut a, again).iter().all(|(x, _)| near(*x, 20.0)));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn list_arguments_are_checked() {
    let dir = workdir("args");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "blank.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "page_add_text", json!({ "doc": doc, "page": 1, "at": [20, 20], "text": "One\nTwo" }));
    assert!(err(&mut a, "text_list_set", json!({ "doc": doc, "page": 1, "block": 1, "style": "square" })).contains("unknown style"));
    assert!(
        err(&mut a, "text_list_set", json!({ "doc": doc, "page": 1, "block": 1, "paragraphs": [2, 5], "style": "disc" })).contains("out of range")
    );
    assert!(err(&mut a, "text_list_set", json!({ "doc": doc, "page": 1, "block": 9, "style": "disc" })).contains("no added item"));
    assert!(err(&mut a, "text_list_set", json!({ "doc": doc, "page": 1, "style": "disc" })).contains("block"));
    assert!(err(&mut a, "text_list_set", json!({ "doc": doc, "page": 1, "block": 1, "style": "decimal", "start": 0 })).contains("start"));
    assert!(err(&mut a, "text_list_remove", json!({ "doc": doc, "page": 1, "block": 1 })).contains("none of those paragraphs"));
    assert!(err(&mut a, "text_list_indent", json!({ "doc": doc, "page": 1, "block": 1, "levels": 0 })).contains("levels"));
    assert!(err(&mut a, "text_list_set", json!({ "doc": doc, "page": 1, "paragraph": 4, "style": "disc" })).contains("out of range"));
    assert_eq!(ok(&mut a, "doc_info", json!({ "doc": doc }))["document"]["undo"], "Add text", "only the added text is undoable");
    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    assert_eq!(ok(&mut a, "doc_info", json!({ "doc": doc }))["document"]["undo"], Value::Null, "failed list edits added no undo steps");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn page_paragraphs_become_a_tagged_list() {
    let dir = workdir("tagged");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "tagged.pdf" }))["doc"].as_u64().unwrap();
    assert_eq!(ok(&mut a, "text_paragraphs", json!({ "doc": doc, "page": 1 }))["count"], 3);

    let out = ok(&mut a, "text_list_set", json!({ "doc": doc, "page": 1, "paragraph": 1, "count": 3, "style": "decimal" }));
    assert_eq!(out["block"], 1, "{out}");
    assert_eq!(out["text"], "Apples\nPears\nPlums", "the typed dash is replaced by the label: {out}");
    assert_eq!(out["tags"]["lists"], 1, "{out}");
    let roles: Vec<&str> = out["tags"]["added"].as_array().unwrap().iter().map(|t| t["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["Lbl", "LBody", "Lbl", "LBody", "Lbl", "LBody"], "{out}");
    let t = text(&mut a, doc);
    assert!(t.contains("1. Apples") && t.contains("2. Pears") && t.contains("3. Plums"), "{t}");
    let l = lines(&mut a, doc);
    assert_eq!(l.len(), 3, "the source paragraphs left the page: {l:?}");
    assert!(l.iter().all(|(x, _)| near(*x, 20.0)), "labels where the paragraphs started: {l:?}");
    let item = |d: u64| [(d, "LI".to_owned()), (d + 1, "Lbl".to_owned()), (d + 1, "LBody".to_owned())];
    let mut want = vec![(0, "Document".to_owned()), (1, "L".to_owned())];
    for _ in 0..3 {
        want.extend(item(2));
    }
    assert_eq!(outline(&mut a, doc), want);
    // Each label and body holds its own marked content; the old paragraphs are gone.
    let tree = ok(&mut a, "tags_tree", json!({ "doc": doc }));
    let mcids: Vec<Value> = tree["elements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["standard"] == "Lbl" || e["standard"] == "LBody")
        .map(|e| e["kids"][0]["mcid"].clone())
        .collect();
    assert_eq!(mcids.len(), 6);
    assert!(mcids.iter().all(Value::is_i64), "{tree}");

    // Nest the last two items: a nested L inside the first item's LBody.
    ok(&mut a, "text_list_indent", json!({ "doc": doc, "page": 1, "block": 1, "paragraphs": [2, 3] }));
    let mut nested = vec![(0, "Document".to_owned()), (1, "L".to_owned())];
    nested.extend(item(2));
    nested.push((4, "L".to_owned()));
    nested.extend(item(5));
    nested.extend(item(5));
    assert_eq!(outline(&mut a, doc), nested);

    // Retyping with a new paragraph continues the nested list, still tagged.
    ok(&mut a, "content_update", json!({ "doc": doc, "page": 1, "index": 1, "text": "Apples\nPears\nPlums\nFigs" }));
    assert_eq!(labels(&mut a, doc), [json!("1."), json!("1."), json!("2."), json!("3.")]);
    let mut four = nested.clone();
    four.extend(item(5));
    assert_eq!(outline(&mut a, doc), four);

    // Undo goes back one step at a time; the tree follows.
    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    assert_eq!(outline(&mut a, doc), nested);
    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    assert_eq!(outline(&mut a, doc), want);
    ok(&mut a, "edit_redo", json!({ "doc": doc }));
    assert_eq!(outline(&mut a, doc), nested);

    // Through save and reopen, and the structure checker accepts the lists.
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "tagged-list.pdf" }));
    let again = ok(&mut a, "doc_open", json!({ "path": "tagged-list.pdf" }))["doc"].as_u64().unwrap();
    assert_eq!(outline(&mut a, again), nested);
    assert_eq!(labels(&mut a, again), [json!("1."), json!("1."), json!("2.")]);
    assert!(text(&mut a, again).contains("2. Plums"));
    let _ = std::fs::remove_dir_all(&dir);
}
