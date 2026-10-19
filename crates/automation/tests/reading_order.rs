//! End-to-end tests of the Reading Order tool and the Order panel
//! (`reading_order_list`, `_tag`, `_move`, `_clear`) on synthetic PDFs: the structure tree,
//! the MCIDs in the content, the accessible text order, undo/redo, save and reopen, and the
//! Accessibility Checker's verdicts.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use pdfcraft_automation::{Automation, Content, ToolError};
use serde_json::{Value, json};

/// Write `objs` (object 1 is the catalog) as a PDF.
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

fn stream(s: &str) -> Vec<u8> {
    format!("<< /Length {} >>\nstream\n{s}\nendstream", s.len()).into_bytes()
}

/// Page 1 (200 × 200, untagged): a title, two paragraph lines in one text object, an image with
/// a caption under it, a 2 × 2 table with a rule, and a footer. Page 2: a label and a text
/// field. Rectangles below are in points from the top-left.
fn untagged_pdf() -> Vec<u8> {
    let c1 = "BT /F1 18 Tf 20 175 Td (Report) Tj ET \
              BT /F1 12 Tf 20 150 Td (First paragraph) Tj 0 -16 Td (Second paragraph) Tj ET \
              q 40 0 0 30 20 90 cm /Im1 Do Q \
              BT /F1 10 Tf 20 76 Td (Figure 1) Tj ET \
              BT /F1 10 Tf 20 50 Td (Name) Tj 80 0 Td (Age) Tj ET \
              BT /F1 10 Tf 20 36 Td (Ann) Tj 80 0 Td (31) Tj ET \
              20 46 m 180 46 l S \
              BT /F1 8 Tf 90 8 Td (Page 1) Tj ET";
    let c2 = "BT /F1 12 Tf 20 150 Td (Name:) Tj ET";
    pdf(&[
        b"<< /Type /Catalog /Pages 2 0 R /Lang (en-US) /AcroForm << /Fields [9 0 R] >> >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R 7 0 R] /Count 2 /MediaBox [0 0 200 200] >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> /XObject << /Im1 6 0 R >> >> >>".to_vec(),
        stream(c1),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        b"<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /Length 1 >>\nstream\n\x7f\nendstream"
            .to_vec(),
        b"<< /Type /Page /Parent 2 0 R /Contents 8 0 R /Annots [9 0 R] /Resources << /Font << /F1 5 0 R >> >> >>".to_vec(),
        stream(c2),
        b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /TU (Your name) /Rect [70 145 170 160] /P 7 0 R /F 4 >>".to_vec(),
    ])
}

const TITLE: [f64; 4] = [10.0, 0.0, 190.0, 33.0];
const PARAGRAPHS: [f64; 4] = [10.0, 34.0, 190.0, 72.0];
const SECOND_LINE: [f64; 4] = [10.0, 55.0, 190.0, 72.0];
const FIGURE: [f64; 4] = [10.0, 75.0, 190.0, 130.0];
const TABLE: [f64; 4] = [10.0, 135.0, 190.0, 170.0];
const FOOTER: [f64; 4] = [10.0, 180.0, 190.0, 200.0];
const FIELD: [f64; 4] = [60.0, 35.0, 180.0, 60.0];
const LABEL: [f64; 4] = [10.0, 35.0, 60.0, 60.0];

fn workdir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pdfcraft-reading-order-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("doc.pdf"), untagged_pdf()).unwrap();
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

/// The regions' standard types, in order.
fn regions(order: &Value) -> Vec<String> {
    order["regions"].as_array().unwrap().iter().map(|r| r["standard"].as_str().unwrap().to_owned()).collect()
}

/// (depth, standard type) of every element, in document order.
fn outline(tree: &Value) -> Vec<(u64, String)> {
    tree["elements"].as_array().unwrap().iter().map(|e| (e["depth"].as_u64().unwrap(), e["standard"].as_str().unwrap().to_owned())).collect()
}

fn element<'a>(tree: &'a Value, standard: &str) -> &'a Value {
    tree["elements"].as_array().unwrap().iter().find(|e| e["standard"] == standard).unwrap_or_else(|| panic!("no {standard} in {tree}"))
}

/// The MCIDs the tree gives page `page` (1-based).
fn tree_mcids(tree: &Value, page: u64) -> Vec<i64> {
    let mut v: Vec<i64> = tree["elements"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|e| e["kids"].as_array().unwrap().iter())
        .filter(|k| k["page"] == page && k["mcid"].is_i64())
        .map(|k| k["mcid"].as_i64().unwrap())
        .collect();
    v.sort();
    v
}

/// The MCIDs written in a page's content (in a saved file), and whether its marked content is
/// balanced.
fn content_mcids(path: &Path, page: usize) -> (Vec<i64>, bool) {
    let doc = pdfcraft_cos::Document::open(std::sync::Arc::new(std::fs::read(path).unwrap())).unwrap();
    let pg = &pdfcraft_model::pages(&doc)[page];
    let data = match &*doc.resolve(pg.dict.get(b"Contents").unwrap()) {
        pdfcraft_cos::Object::Stream(s) => s.decoded().unwrap(),
        other => panic!("one content stream expected, got {other:?}"),
    };
    let text = String::from_utf8_lossy(&data).into_owned();
    let tokens: Vec<&str> = text.split(|c: char| c.is_whitespace() || c == '<' || c == '>').filter(|t| !t.is_empty()).collect();
    let mut mcids: Vec<i64> = tokens.windows(2).filter(|w| w[0] == "/MCID").map(|w| w[1].parse().unwrap()).collect();
    let opens = tokens.iter().filter(|t| **t == "BDC" || **t == "BMC").count();
    let closes = tokens.iter().filter(|t| **t == "EMC").count();
    mcids.sort();
    (mcids, opens == closes)
}

fn check(a: &mut Automation, doc: u64) -> Value {
    let r = ok(a, "accessibility_check", json!({ "doc": doc }));
    let mut out = serde_json::Map::new();
    for x in r["results"].as_array().unwrap() {
        out.insert(x["rule"].as_str().unwrap().to_owned(), x["status"].clone());
    }
    Value::Object(out)
}

fn accessible_text(a: &mut Automation, doc: u64) -> String {
    ok(a, "doc_export_accessible_text", json!({ "doc": doc }))["text"].as_str().unwrap().to_owned()
}

/// `parts` appear in `text` in this order.
fn in_order(text: &str, parts: &[&str]) {
    let mut at = 0;
    for p in parts {
        let i = text[at..].find(p).unwrap_or_else(|| panic!("{p:?} missing (in order) from {text:?}"));
        at += i + p.len();
    }
}

#[test]
fn regions_are_tagged_split_reordered_and_cleared_through_tools() {
    let dir = workdir("e2e");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "doc.pdf" }))["doc"].as_u64().unwrap();

    // Untagged: no regions, everything untagged; the checker says so.
    let o = ok(&mut a, "reading_order_list", json!({ "doc": doc, "page": 1 }));
    assert_eq!((o["tagged"].as_bool(), o["regions"].as_array().map(Vec::len)), (Some(false), Some(0)));
    assert_eq!(o["untagged"], 11, "{o}");
    assert_eq!(check(&mut a, doc)["tagged-pdf"], "failed");

    // Title as a heading: the document gets a tree.
    let r = ok(&mut a, "reading_order_tag", json!({ "doc": doc, "page": 1, "rect": TITLE, "role": "h1" }));
    assert_eq!((r["created_tree"].as_bool(), r["wrapped"].as_u64(), r["mcids"].clone()), (Some(true), Some(1), json!([0])), "{r}");
    assert_eq!(regions(&r["order"]), ["H1"]);
    let rect = r["order"]["regions"][0]["rect"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>();
    assert!(rect[0] >= 19.0 && rect[1] > 0.0 && rect[3] <= 33.0, "title box from its glyphs: {rect:?}");

    // Both paragraph lines (one run in one text object) become one P with one MCID.
    let r = ok(&mut a, "reading_order_tag", json!({ "doc": doc, "page": 1, "rect": PARAGRAPHS, "role": "text" }));
    assert_eq!((r["units"].as_u64(), r["mcids"].clone()), (Some(2), json!([1])), "{r}");
    // Image and caption: a Figure with alternate text holding a Caption.
    let r = ok(&mut a, "reading_order_tag", json!({ "doc": doc, "page": 1, "rect": FIGURE, "role": "figure-caption", "alt": "A bar chart" }));
    assert_eq!(r["elements"].as_array().unwrap().len(), 2, "{r}");
    // The table: rows and cells from the layout, the rule an artifact.
    let r = ok(&mut a, "reading_order_tag", json!({ "doc": doc, "page": 1, "rect": TABLE, "role": "table" }));
    assert_eq!((r["units"].as_u64(), r["artifacts"].as_u64()), (Some(5), Some(1)), "{r}");
    // The footer is decoration.
    let r = ok(&mut a, "reading_order_tag", json!({ "doc": doc, "page": 1, "rect": FOOTER, "role": "background", "artifact_subtype": "Footer" }));
    assert_eq!((r["artifacts"].as_u64(), r["elements"].clone()), (Some(1), json!([])), "{r}");
    assert_eq!(regions(&r["order"]), ["H1", "P", "Figure", "Table"]);
    assert_eq!((r["order"]["untagged"].as_u64(), r["order"]["artifacts"].as_u64()), (Some(0), Some(2)));

    let t = ok(&mut a, "tags_tree", json!({ "doc": doc }));
    assert_eq!(
        outline(&t),
        [
            (0, "Document"),
            (1, "H1"),
            (1, "P"),
            (1, "Figure"),
            (2, "Caption"),
            (1, "Table"),
            (2, "TR"),
            (3, "TH"),
            (3, "TH"),
            (2, "TR"),
            (3, "TD"),
            (3, "TD"),
        ]
        .map(|(d, s)| (d, s.to_owned()))
    );
    assert_eq!(element(&t, "Figure")["alt"], "A bar chart");
    assert_eq!(tree_mcids(&t, 1), (0..8).collect::<Vec<_>>(), "one MCID per tagged sequence");

    // Page 2: the label as text, the field as a Form element.
    ok(&mut a, "reading_order_tag", json!({ "doc": doc, "page": 2, "rect": LABEL, "role": "text" }));
    let r = ok(&mut a, "reading_order_tag", json!({ "doc": doc, "page": 2, "rect": FIELD, "role": "form-field" }));
    assert_eq!(r["widgets"], 1, "{r}");
    assert_eq!(regions(&r["order"]), ["P", "Form"]);
    let t = ok(&mut a, "tags_tree", json!({ "doc": doc }));
    let form = element(&t, "Form");
    assert_eq!((form["alt"].as_str(), form["kids"][0]["object"].as_u64()), (Some("Your name"), Some(9)));

    let c = check(&mut a, doc);
    for rule in [
        "tagged-pdf",
        "tagged-content",
        "tagged-annotations",
        "tagged-form-fields",
        "figures-alt-text",
        "table-headers",
        "table-regularity",
        "table-rows",
        "table-cells",
        "heading-nesting",
    ] {
        assert_eq!(c[rule], "passed", "{rule}: {c}");
    }
    let text = accessible_text(&mut a, doc);
    in_order(&text, &["Report", "First paragraph", "Second paragraph", "A bar chart", "Name", "Age", "Ann", "31", "Name:"]);
    assert!(!text.contains("Page 1"), "the footer artifact is left out: {text:?}");

    // Split: the second line alone becomes a heading. The P keeps its MCID, the heading gets a
    // new one; the heading goes after the P.
    let r = ok(&mut a, "reading_order_tag", json!({ "doc": doc, "page": 1, "rect": SECOND_LINE, "role": "h2" }));
    assert_eq!((r["split"].as_u64(), r["moved"].as_u64(), r["mcids"].clone()), (Some(1), Some(0), json!([8])), "{r}");
    assert_eq!(regions(&r["order"]), ["H1", "P", "H2", "Figure", "Table"]);
    let p = &r["order"]["regions"][1];
    assert_eq!(p["elements"][0]["mcids"], json!([1]));
    let t = ok(&mut a, "tags_tree", json!({ "doc": doc }));
    assert_eq!(tree_mcids(&t, 1), (0..9).collect::<Vec<_>>());
    in_order(&accessible_text(&mut a, doc), &["Report", "First paragraph", "Second paragraph", "A bar chart"]);

    // Order panel: the heading moves up before the paragraph (region 2), as one undo step.
    let h2 = r["order"]["regions"][2]["obj"].as_u64().unwrap();
    let m = ok(&mut a, "reading_order_move", json!({ "doc": doc, "element": h2, "page": 1, "to": 2 }));
    assert_eq!(regions(&m["order"]), ["H1", "H2", "P", "Figure", "Table"]);
    in_order(&accessible_text(&mut a, doc), &["Report", "Second paragraph", "First paragraph", "A bar chart"]);
    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    assert_eq!(regions(&ok(&mut a, "reading_order_list", json!({ "doc": doc, "page": 1 }))), ["H1", "P", "H2", "Figure", "Table"]);
    ok(&mut a, "edit_redo", json!({ "doc": doc }));
    assert_eq!(regions(&ok(&mut a, "reading_order_list", json!({ "doc": doc, "page": 1 }))), ["H1", "H2", "P", "Figure", "Table"]);
    // Moved back by obj: after the paragraph.
    let p_obj = ok(&mut a, "reading_order_list", json!({ "doc": doc, "page": 1 }))["regions"][2]["obj"].as_u64().unwrap();
    let m = ok(&mut a, "reading_order_move", json!({ "doc": doc, "element": h2, "after": p_obj, "page": 1 }));
    assert_eq!(regions(&m["order"]), ["H1", "P", "H2", "Figure", "Table"]);

    // Undo and redo of a region tag: the split comes and goes as one step.
    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    let o = ok(&mut a, "reading_order_list", json!({ "doc": doc, "page": 1 }));
    assert_eq!(regions(&o), ["H1", "P", "Figure", "Table"], "the split undone");
    ok(&mut a, "edit_redo", json!({ "doc": doc }));
    ok(&mut a, "edit_redo", json!({ "doc": doc }));
    ok(&mut a, "edit_redo", json!({ "doc": doc }));
    assert_eq!(regions(&ok(&mut a, "reading_order_list", json!({ "doc": doc, "page": 1 }))), ["H1", "P", "H2", "Figure", "Table"]);

    // Saved and reopened: the same tree, the content's MCIDs match the tree's.
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "tagged.pdf" }));
    let (mcids, balanced) = content_mcids(&dir.join("tagged.pdf"), 0);
    assert!(balanced);
    assert_eq!(mcids, (0..9).collect::<Vec<_>>(), "every MCID once in the content");
    let re = ok(&mut a, "doc_open", json!({ "path": "tagged.pdf" }))["doc"].as_u64().unwrap();
    let t2 = ok(&mut a, "tags_tree", json!({ "doc": re }));
    assert_eq!(outline(&t2), outline(&ok(&mut a, "tags_tree", json!({ "doc": doc }))));
    assert_eq!(tree_mcids(&t2, 1), mcids);
    assert_eq!(regions(&ok(&mut a, "reading_order_list", json!({ "doc": re, "page": 1 }))), ["H1", "P", "H2", "Figure", "Table"]);
    let c = check(&mut a, re);
    assert_eq!((c["tagged-content"].as_str(), c["heading-nesting"].as_str()), (Some("passed"), Some("passed")), "{c}");

    // Clear page structure: page 1 is untagged again; the checker fails tagged content; undo.
    let r = ok(&mut a, "reading_order_clear", json!({ "doc": re, "page": 1 }));
    assert_eq!((r["mcids"].as_u64(), r["artifacts"].as_u64()), (Some(9), Some(2)), "{r}");
    assert_eq!((r["order"]["regions"].as_array().map(Vec::len), r["order"]["untagged"].as_u64()), (Some(0), Some(11)));
    let t = ok(&mut a, "tags_tree", json!({ "doc": re }));
    assert_eq!(outline(&t), [(0, "Document"), (1, "P"), (1, "Form")].map(|(d, s)| (d, s.to_owned())), "page 2's tags stay");
    assert_eq!(check(&mut a, re)["tagged-content"], "failed");
    assert!(matches!(a.call("reading_order_clear", &json!({ "doc": re, "page": 1 })), Err(ToolError::Failed(_))), "nothing left to clear");
    ok(&mut a, "edit_undo", json!({ "doc": re }));
    assert_eq!(regions(&ok(&mut a, "reading_order_list", json!({ "doc": re, "page": 1 }))), ["H1", "P", "H2", "Figure", "Table"]);
    assert_eq!(check(&mut a, re)["tagged-content"], "passed");

    // Cleared and saved, then retagged: still consistent.
    ok(&mut a, "reading_order_clear", json!({ "doc": re, "page": 1, "keep_artifacts": true }));
    ok(&mut a, "reading_order_tag", json!({ "doc": re, "page": 1, "rect": [0, 0, 200, 179], "role": "text" }));
    ok(&mut a, "doc_save", json!({ "doc": re, "path": "retagged.pdf" }));
    let (mcids, balanced) = content_mcids(&dir.join("retagged.pdf"), 0);
    assert!(balanced);
    let re2 = ok(&mut a, "doc_open", json!({ "path": "retagged.pdf" }))["doc"].as_u64().unwrap();
    let t = ok(&mut a, "tags_tree", json!({ "doc": re2 }));
    assert_eq!(tree_mcids(&t, 1), mcids);
    assert_eq!(regions(&ok(&mut a, "reading_order_list", json!({ "doc": re2, "page": 1 }))), ["P"]);
    assert_eq!(check(&mut a, re2)["tagged-content"], "passed", "the footer artifact was kept");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn artifacts_and_tagged_content_move_as_a_whole_or_split() {
    let dir = workdir("whole");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "doc.pdf" }))["doc"].as_u64().unwrap();
    // The whole page as text first: one P.
    let r = ok(&mut a, "reading_order_tag", json!({ "doc": doc, "page": 1, "rect": [0, 0, 200, 200], "role": "text" }));
    assert_eq!(regions(&r["order"]), ["P"]);
    let before = r["mcids"].as_array().unwrap().len();
    // The footer, tagged, becomes an artifact as a whole: its MCID leaves the P.
    let r = ok(&mut a, "reading_order_tag", json!({ "doc": doc, "page": 1, "rect": FOOTER, "role": "background" }));
    assert_eq!((r["moved"].as_u64(), r["split"].as_u64()), (Some(1), Some(0)), "{r}");
    let p = &r["order"]["regions"][0]["elements"][0];
    assert_eq!(p["mcids"].as_array().unwrap().len(), before - 1);
    // ... and back to text as a whole (an artifact gets a new MCID).
    let r = ok(&mut a, "reading_order_tag", json!({ "doc": doc, "page": 1, "rect": FOOTER, "role": "text" }));
    assert_eq!((r["moved"].as_u64(), r["mcids"].as_array().map(Vec::len)), (Some(1), Some(1)), "{r}");
    assert_eq!(regions(&r["order"]), ["P", "P"]);
    // The second paragraph line out of its run: split, the rest stays in the first P.
    let r = ok(&mut a, "reading_order_tag", json!({ "doc": doc, "page": 1, "rect": SECOND_LINE, "role": "h1" }));
    assert_eq!(r["split"], 1, "{r}");
    assert_eq!(regions(&r["order"]), ["P", "H1", "P"], "the new heading goes after the content before it");
    let first = r["order"]["regions"][0]["obj"].as_u64().unwrap();
    let h1 = r["order"]["regions"][1]["obj"].as_u64().unwrap();
    let m = ok(&mut a, "reading_order_move", json!({ "doc": doc, "element": h1, "before": first, "page": 1 }));
    assert_eq!(regions(&m["order"]), ["H1", "P", "P"]);
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "out.pdf" }));
    let (mcids, balanced) = content_mcids(&dir.join("out.pdf"), 0);
    assert!(balanced);
    let distinct: BTreeSet<i64> = mcids.iter().copied().collect();
    assert_eq!(distinct.len(), mcids.len(), "no MCID twice");
    let t = ok(&mut a, "tags_tree", json!({ "doc": doc }));
    assert_eq!(tree_mcids(&t, 1), mcids);
    let r = ok(&mut a, "accessibility_check", json!({ "doc": doc, "pages": [1], "rules": ["tagged-content"] }));
    assert_eq!(r["results"].as_array().unwrap().iter().find(|x| x["rule"] == "tagged-content").unwrap()["status"], "passed", "page 1: {r}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn bad_reading_order_requests_change_nothing() {
    let dir = workdir("bad");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "doc.pdf" }))["doc"].as_u64().unwrap();
    for b in [
        json!({ "doc": doc, "page": 1, "rect": TITLE, "role": "bogus" }),
        json!({ "doc": doc, "page": 1, "rect": [1, 2, 3], "role": "text" }),
        json!({ "doc": doc, "page": 1, "role": "text" }),
        json!({ "doc": doc, "page": 9, "rect": TITLE, "role": "text" }),
        json!({ "doc": doc, "page": 1, "rect": TITLE, "role": "background", "artifact_type": "Nope" }),
    ] {
        assert!(matches!(a.call("reading_order_tag", &b), Err(ToolError::InvalidArgs(_))), "{b}");
    }
    for b in [
        json!({ "doc": doc, "page": 1, "rect": [5, 5, 5, 50], "role": "text" }),
        json!({ "doc": doc, "page": 1, "rect": [150, 100, 190, 130], "role": "text" }),
        json!({ "doc": doc, "page": 1, "rect": TITLE, "role": "text", "alt": "not for text" }),
        json!({ "doc": doc, "page": 1, "rect": TITLE, "role": "h1", "artifact_type": "Layout" }),
        json!({ "doc": doc, "page": 1, "rect": TITLE, "role": "figure-caption" }),
        json!({ "doc": doc, "page": 1, "rect": TITLE, "role": "form-field" }),
        json!({ "doc": doc, "page": 1, "rect": FOOTER, "role": "background", "artifact_type": "Layout", "artifact_subtype": "Footer" }),
    ] {
        assert!(matches!(a.call("reading_order_tag", &b), Err(ToolError::Failed(_))), "{b}");
    }
    assert!(matches!(a.call("reading_order_clear", &json!({ "doc": doc, "page": 1 })), Err(ToolError::Failed(_))));
    assert!(matches!(a.call("reading_order_move", &json!({ "doc": doc, "element": 3, "before": 4 })), Err(ToolError::Failed(_))));
    assert!(matches!(a.call("reading_order_list", &json!({ "doc": doc, "page": 0 })), Err(ToolError::InvalidArgs(_))));
    assert_eq!(ok(&mut a, "doc_list", json!({}))["documents"][0]["dirty"], false);

    // Moves that would tangle the tree are refused.
    let r = ok(&mut a, "reading_order_tag", json!({ "doc": doc, "page": 1, "rect": TABLE, "role": "table" }));
    let els = r["elements"].as_array().unwrap();
    let (table, cell) = (els[0].as_u64().unwrap(), els.last().unwrap().as_u64().unwrap());
    for b in [
        json!({ "doc": doc, "element": table, "before": cell }),
        json!({ "doc": doc, "element": table }),
        json!({ "doc": doc, "element": table, "before": cell, "after": cell }),
        json!({ "doc": doc, "element": table, "to": 1 }),
        json!({ "doc": doc, "element": table, "page": 1, "to": 7 }),
    ] {
        assert!(a.call("reading_order_move", &b).is_err(), "{b}");
    }
    std::fs::remove_dir_all(dir).unwrap();
}
