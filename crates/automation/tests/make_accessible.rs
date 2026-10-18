//! `accessibility_make_accessible` end to end on synthetic PDFs: an untagged text document with a figure
//! and a form field, an already tagged one, an image-only scan, bad arguments and one-step undo.

use std::path::{Path, PathBuf};

use pdfcraft_automation::{Automation, Content, ToolError};
use serde_json::{Value, json};

/// A PDF from its objects (object 1 is the catalog), with `info` as object `objs.len() + 1`.
fn pdf(objs: &[Vec<u8>], info: Option<&str>) -> Vec<u8> {
    let mut objs = objs.to_vec();
    if let Some(i) = info {
        objs.push(i.as_bytes().to_vec());
    }
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
    let info_ref = if info.is_some() { format!(" /Info {} 0 R", objs.len()) } else { String::new() };
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R{info_ref} >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

fn stream(content: &str) -> Vec<u8> {
    format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()).into_bytes()
}

const IMAGE: &[u8] = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /BitsPerComponent 8 /ColorSpace /DeviceGray /Length 4 >>\nstream\n\x10\x80\x80\x10\nendstream";

/// Untagged, no title, no language: an H1-sized heading, two paragraphs, an image and an
/// AcroForm text field without a tooltip.
fn untagged_pdf() -> Vec<u8> {
    let c = "BT /F1 24 Tf 40 520 Td (Annual Report) Tj ET \
             BT /F1 12 Tf 40 480 Td (This year the garden grew more tomatoes than ever before.) Tj ET \
             BT /F1 12 Tf 40 462 Td (The second paragraph explains why the beans did not.) Tj ET \
             q 120 0 0 90 40 300 cm /Im1 Do Q";
    pdf(
        &[
            b"<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [7 0 R] /DA (/Helv 0 Tf 0 g) >> >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 400 600] >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Annots [7 0 R] /Resources << /Font << /F1 5 0 R >> /XObject << /Im1 6 0 R >> >> >>"
                .to_vec(),
            stream(c),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            IMAGE.to_vec(),
            b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /Rect [40 200 240 224] /P 3 0 R /F 4 >>".to_vec(),
        ],
        None,
    )
}

/// Tagged with a language and a title: H1 text (MCID 0) and a figure (MCID 1, no alt).
fn tagged_pdf() -> Vec<u8> {
    let c = "/H1 << /MCID 0 >> BDC BT /F1 12 Tf 20 180 Td (Introduction) Tj ET EMC \
             /Figure << /MCID 1 >> BDC q 40 0 0 30 20 100 cm /Im1 Do Q EMC";
    pdf(
        &[
            b"<< /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 6 0 R /Lang (en-US) >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 200] >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R /StructParents 0 /Resources << /Font << /F1 5 0 R >> /XObject << /Im1 10 0 R >> >> >>"
                .to_vec(),
            stream(c),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            b"<< /Type /StructTreeRoot /K 7 0 R /ParentTree 11 0 R /ParentTreeNextKey 1 >>".to_vec(),
            b"<< /Type /StructElem /S /Document /P 6 0 R /K [8 0 R 9 0 R] >>".to_vec(),
            b"<< /Type /StructElem /S /H1 /P 7 0 R /Pg 3 0 R /K 0 >>".to_vec(),
            b"<< /Type /StructElem /S /Figure /P 7 0 R /Pg 3 0 R /K 1 >>".to_vec(),
            IMAGE.to_vec(),
            b"<< /Nums [0 [8 0 R 9 0 R]] >>".to_vec(),
        ],
        Some("<< /Title (Tagged sample) >>"),
    )
}

/// A scan: one page that only draws an image, with a language.
fn scan_pdf() -> Vec<u8> {
    pdf(
        &[
            b"<< /Type /Catalog /Pages 2 0 R /Lang (en-US) >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 300 400] >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /XObject << /Im1 5 0 R >> >> >>".to_vec(),
            stream("q 300 0 0 400 0 0 cm /Im1 Do Q"),
            IMAGE.to_vec(),
        ],
        None,
    )
}

fn workdir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pdfcraft-makeacc-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("report.pdf"), untagged_pdf()).unwrap();
    std::fs::write(dir.join("tagged.pdf"), tagged_pdf()).unwrap();
    std::fs::write(dir.join("scan.pdf"), scan_pdf()).unwrap();
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

fn open(a: &mut Automation, path: &str) -> u64 {
    ok(a, "doc_open", json!({ "path": path }))["doc"].as_u64().unwrap()
}

fn step<'a>(r: &'a Value, name: &str) -> &'a Value {
    r["steps"].as_array().unwrap().iter().find(|s| s["step"] == name).unwrap_or_else(|| panic!("no step {name}: {r}"))
}

fn rule(a: &mut Automation, doc: u64, id: &str) -> String {
    let r = ok(a, "accessibility_check", json!({ "doc": doc }));
    r["results"].as_array().unwrap().iter().find(|x| x["rule"] == id).unwrap_or_else(|| panic!("no rule {id}: {r}"))["status"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn failed_rules(v: &Value) -> Vec<String> {
    serde_json::from_value(v["checker"]["failed"].clone()).unwrap()
}

fn dirty(a: &mut Automation) -> bool {
    ok(a, "doc_list", json!({}))["documents"][0]["dirty"].as_bool().unwrap()
}

#[test]
fn an_untagged_text_pdf_becomes_tagged_titled_and_in_structure_order() {
    let dir = workdir("untagged");
    let mut a = auto(&dir);
    let doc = open(&mut a, "report.pdf");
    for (id, want) in [("tagged-pdf", "failed"), ("title", "failed"), ("primary-language", "failed")] {
        assert_eq!(rule(&mut a, doc, id), want, "{id} before");
    }
    let r = ok(&mut a, "accessibility_make_accessible", json!({ "doc": doc, "lang": "en-US", "field_tooltips": { "name": "Your full name" } }));
    let order: Vec<&str> = r["steps"].as_array().unwrap().iter().map(|s| s["step"].as_str().unwrap()).collect();
    assert_eq!(order, ["language", "ocr", "field_tooltips", "autotag", "title", "alt_text", "tables", "tab_order", "full_check"]);
    assert_eq!(step(&r, "language")["status"], "done", "{r}");
    assert_eq!(step(&r, "ocr")["status"], "skipped", "the page has text: {r}");
    assert_eq!(step(&r, "ocr")["image_only_pages"], json!([]));
    assert_eq!(step(&r, "autotag")["status"], "done", "{r}");
    assert_eq!(step(&r, "autotag")["form_fields"], 1, "the field is tagged: {r}");
    assert_eq!(step(&r, "autotag")["fields_needing_tooltip"], json!([]), "its tooltip was set first: {r}");
    assert_eq!(step(&r, "title")["status"], "done", "{r}");
    assert_eq!(step(&r, "title")["source"], "first H1");
    assert_eq!(step(&r, "title")["title"], "Annual Report");
    assert_eq!(step(&r, "alt_text")["status"], "skipped");
    assert_eq!(step(&r, "field_tooltips")["status"], "done", "{r}");
    assert_eq!(step(&r, "tab_order")["status"], "done", "{r}");
    assert_eq!((r["changed"].as_bool(), r["undo"].as_str()), (Some(true), Some("Make accessible")));

    // The checker improved.
    let (before, after) = (failed_rules(&r["before"]), failed_rules(&r["after"]));
    assert!(after.len() < before.len(), "before {before:?}, after {after:?}");
    for id in ["tagged-pdf", "title", "primary-language", "tab-order", "field-descriptions", "tagged-form-fields"] {
        assert!(
            before.iter().any(|f| f == id) || ["field-descriptions", "tab-order", "tagged-form-fields"].contains(&id),
            "{id} failed before: {before:?}"
        );
        assert!(!after.iter().any(|f| f == id), "{id} still fails: {after:?}");
        assert_eq!(rule(&mut a, doc, id), "passed", "{id} after");
    }
    let info = ok(&mut a, "doc_info", json!({ "doc": doc }));
    assert_eq!(info["title"], "Annual Report", "{info}");
    let view = ok(&mut a, "doc_initial_view", json!({ "doc": doc }))["initial_view"].clone();
    assert_eq!((view["display_title"].as_bool(), view["language"].as_str()), (Some(true), Some("en-US")), "{view}");
    let tree = ok(&mut a, "tags_tree", json!({ "doc": doc }));
    assert_eq!(tree["tagged"], true);
    assert_eq!(tree["lang"], "en-US");

    // What still needs a person: the figure's alternate text and the reading order.
    let review = &r["needs_review"];
    assert_eq!(review["figures_without_alt"]["count"], 1, "{review}");
    let figure = review["figures_without_alt"]["figures"][0]["figure"].as_u64().unwrap();
    assert_eq!(review["fields_without_tooltips"]["count"], 0, "{review}");
    assert_eq!(review["reading_order"]["verify"], true);
    assert!(review["manual_checks"].as_array().unwrap().iter().any(|m| m == "logical-reading-order"), "{review}");

    // One undo step restores everything, and redo brings it all back.
    assert_eq!(ok(&mut a, "edit_undo", json!({ "doc": doc }))["undone"], "Make accessible");
    assert!(a.call("edit_undo", &json!({ "doc": doc })).is_err(), "the whole action was one step");
    for (id, want) in [("tagged-pdf", "failed"), ("title", "failed"), ("primary-language", "failed")] {
        assert_eq!(rule(&mut a, doc, id), want, "{id} after undo");
    }
    assert_eq!(ok(&mut a, "doc_info", json!({ "doc": doc }))["title"], Value::Null);
    assert_eq!(ok(&mut a, "doc_initial_view", json!({ "doc": doc }))["initial_view"]["display_title"], false);
    assert_eq!(ok(&mut a, "tags_tree", json!({ "doc": doc }))["tagged"], false);
    assert_eq!(ok(&mut a, "edit_redo", json!({ "doc": doc }))["redone"], "Make accessible");
    assert_eq!(rule(&mut a, doc, "tagged-pdf"), "passed");
    ok(&mut a, "edit_undo", json!({ "doc": doc }));

    // Without tooltips, the untitled field is listed with one suggested from its name.
    let r = ok(&mut a, "accessibility_make_accessible", json!({ "doc": doc, "lang": "en-US" }));
    assert_eq!(r["needs_review"]["fields_without_tooltips"]["fields"], json!(["name"]));
    assert_eq!(step(&r, "autotag")["fields_needing_tooltip"][0]["name"], "name", "{r}");
    assert!(step(&r, "autotag")["fields_needing_tooltip"][0]["suggested"].is_string(), "{r}");
    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    // The same arguments again tag the same objects, so the figure listed above takes its
    // alternate text in the same call; and a given title wins.
    let r = ok(
        &mut a,
        "accessibility_make_accessible",
        json!({ "doc": doc, "lang": "en-US", "title": "Garden report 2026", "field_tooltips": { "name": "Your full name" },
                "alt_text": { figure.to_string(): "A bar chart of the harvest" } }),
    );
    assert_eq!(step(&r, "alt_text")["status"], "done", "{r}");
    assert_eq!(step(&r, "alt_text")["set"], json!([figure]));
    assert_eq!(step(&r, "title")["source"], "given");
    assert_eq!(r["needs_review"]["figures_without_alt"]["count"], 0, "{r}");
    assert_eq!(r["needs_review"]["fields_without_tooltips"]["count"], 0, "{r}");
    assert_eq!(rule(&mut a, doc, "figures-alt-text"), "passed");

    // Saved and reopened, it stays that way.
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "out/report.pdf" }));
    let mut b = auto(&dir);
    let re = open(&mut b, "out/report.pdf");
    for id in ["tagged-pdf", "title", "primary-language", "tab-order", "figures-alt-text"] {
        assert_eq!(rule(&mut b, re, id), "passed", "{id} after reopening");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_tagged_pdf_keeps_its_tags_with_autotag_auto_and_takes_alt_text() {
    let dir = workdir("tagged");
    let mut a = auto(&dir);
    let doc = open(&mut a, "tagged.pdf");
    let tree = ok(&mut a, "tags_tree", json!({ "doc": doc }));
    let figure = tree["elements"].as_array().unwrap().iter().find(|e| e["type"] == "Figure").unwrap()["obj"].as_u64().unwrap();
    // No lang needed: the document has one.
    let r = ok(&mut a, "accessibility_make_accessible", json!({ "doc": doc, "alt_text": { figure.to_string(): "A photograph of the team" } }));
    assert_eq!(step(&r, "language")["status"], "skipped", "{r}");
    assert_eq!(step(&r, "autotag")["status"], "skipped", "{r}");
    assert!(step(&r, "autotag")["reason"].as_str().unwrap().contains("already tagged"));
    assert_eq!(step(&r, "title")["source"], "existing", "{r}");
    assert_eq!(step(&r, "title")["status"], "done", "DisplayDocTitle was missing: {r}");
    assert_eq!(step(&r, "alt_text")["status"], "done", "{r}");
    assert_eq!(step(&r, "field_tooltips")["status"], "skipped");
    assert_eq!(step(&r, "tab_order")["status"], "done");
    let after = ok(&mut a, "tags_tree", json!({ "doc": doc }));
    let shape = |t: &Value| t["elements"].as_array().unwrap().iter().map(|e| (e["obj"].clone(), e["type"].clone())).collect::<Vec<_>>();
    assert_eq!(shape(&after), shape(&tree), "the tags are left as they are");
    let alt = after["elements"].as_array().unwrap().iter().find(|e| e["obj"] == figure).unwrap()["alt"].clone();
    assert_eq!(alt, "A photograph of the team");
    assert_eq!(r["needs_review"]["figures_without_alt"]["count"], 0, "{r}");

    // A figure number that isn't there fails that step alone; the later steps still run.
    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    let r = ok(&mut a, "accessibility_make_accessible", json!({ "doc": doc, "alt_text": { "999": "Nothing" } }));
    assert_eq!(step(&r, "alt_text")["status"], "failed", "{r}");
    assert_eq!(step(&r, "alt_text")["errors"][0]["figure"], 999);
    assert_eq!(step(&r, "tab_order")["status"], "done", "{r}");
    assert_eq!(r["undo"], "Make accessible");

    // Run again on the result: nothing left to change but the tab order.
    let again = ok(&mut a, "accessibility_make_accessible", json!({ "doc": doc }));
    assert_eq!(step(&again, "title")["status"], "skipped", "{again}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn ocr_auto_skips_scans_when_no_models_are_installed() {
    let dir = workdir("scan");
    let mut a = auto(&dir);
    if ok(&mut a, "ocr_status", json!({}))["available"] == true {
        eprintln!("OCR models are installed: the no-models path can't be tested here");
        return;
    }
    let doc = open(&mut a, "scan.pdf");
    let r = ok(&mut a, "accessibility_make_accessible", json!({ "doc": doc }));
    let ocr = step(&r, "ocr");
    assert_eq!(ocr["status"], "skipped", "{r}");
    assert_eq!(ocr["image_only_pages"], json!([1]));
    assert!(ocr["reason"].as_str().unwrap().contains("no OCR models"), "{ocr}");
    // The rest still ran: the image is now a figure that needs alternate text.
    assert_eq!(step(&r, "autotag")["status"], "done", "{r}");
    assert_eq!(step(&r, "title")["source"], "file name");
    assert_eq!(step(&r, "title")["title"], "scan");
    assert!(failed_rules(&r["after"]).iter().any(|f| f == "image-only"), "still a scan: {r}");

    // always: without models the step fails, and the later steps still run.
    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    let r = ok(&mut a, "accessibility_make_accessible", json!({ "doc": doc, "ocr": "always" }));
    assert_eq!(step(&r, "ocr")["status"], "failed", "{r}");
    assert_eq!(step(&r, "autotag")["status"], "done", "{r}");
    assert_eq!(step(&r, "tab_order")["status"], "done", "{r}");
    let r = ok(&mut a, "accessibility_make_accessible", json!({ "doc": doc, "ocr": "never", "autotag": "never" }));
    assert_eq!(step(&r, "ocr")["status"], "skipped");
    assert_eq!(step(&r, "autotag")["status"], "skipped");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn bad_arguments_change_nothing() {
    let dir = workdir("bad");
    let mut a = auto(&dir);
    let doc = open(&mut a, "report.pdf");
    let invalid = |a: &mut Automation, args: Value| match a.call("accessibility_make_accessible", &args) {
        Err(ToolError::InvalidArgs(m)) => m,
        other => panic!("{args}: expected invalid arguments, got {other:?}"),
    };
    // No language and none given: refused, not guessed.
    assert!(invalid(&mut a, json!({ "doc": doc })).contains("lang"));
    for args in [
        json!({ "doc": doc, "lang": "en US" }),
        json!({ "doc": doc, "lang": "" }),
        json!({ "doc": doc, "lang": 7 }),
        json!({ "doc": doc, "lang": "en-US", "title": "  " }),
        json!({ "doc": doc, "lang": "en-US", "title": 3 }),
        json!({ "doc": doc, "lang": "en-US", "title": "x".repeat(10_001) }),
        json!({ "doc": doc, "lang": "en-US", "ocr": "sometimes" }),
        json!({ "doc": doc, "lang": "en-US", "ocr": true }),
        json!({ "doc": doc, "lang": "en-US", "autotag": "yes" }),
        json!({ "doc": doc, "lang": "en-US", "alt_text": [1] }),
        json!({ "doc": doc, "lang": "en-US", "alt_text": { "fig": "x" } }),
        json!({ "doc": doc, "lang": "en-US", "alt_text": { "0": "x" } }),
        json!({ "doc": doc, "lang": "en-US", "alt_text": { "-4": "x" } }),
        json!({ "doc": doc, "lang": "en-US", "alt_text": { "99999999999": "x" } }),
        json!({ "doc": doc, "lang": "en-US", "alt_text": { "5": "" } }),
        json!({ "doc": doc, "lang": "en-US", "alt_text": { "5": 5 } }),
        json!({ "doc": doc, "lang": "en-US", "field_tooltips": { "nope": "x" } }),
        json!({ "doc": doc, "lang": "en-US", "field_tooltips": { "name": "" } }),
        json!({ "doc": doc, "lang": "en-US", "field_tooltips": "name" }),
        json!({ "doc": doc, "lang": "en-US", "pages": [1] }),
        json!({ "lang": "en-US" }),
    ] {
        invalid(&mut a, args);
    }
    let many: serde_json::Map<String, Value> = (1..=10_001).map(|i| (i.to_string(), json!("x"))).collect();
    invalid(&mut a, json!({ "doc": doc, "lang": "en-US", "alt_text": many }));
    assert!(matches!(a.call("accessibility_make_accessible", &json!({ "doc": 999, "lang": "en-US" })), Err(ToolError::Failed(_))));
    assert!(!dirty(&mut a), "nothing changed");
    assert!(a.call("edit_undo", &json!({ "doc": doc })).is_err(), "no undo step");
    let _ = std::fs::remove_dir_all(dir);
}
