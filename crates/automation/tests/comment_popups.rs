//! End-to-end tests of comment pop-ups (`comment_popup`, `comment_popups_all`, the `popup` of
//! `comment_list`) on synthetic PDFs: open and close, move and resize, undo, save and reopen,
//! rotated pages and hostile values.

use std::path::{Path, PathBuf};

use pdfcraft_automation::{Automation, Content};
use serde_json::{Value, json};

fn pdf(objs: &[&[u8]]) -> Vec<u8> {
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

/// Page 1: 400 × 400, upright. Page 2: 400 × 300 turned 90°, with a broken pop-up (/Rect of
/// strings, /Open a name) on a highlight.
fn doc_pdf() -> Vec<u8> {
    pdf(&[
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 400] >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 300] /Rotate 90 /Annots [5 0 R 6 0 R] >>",
        b"<< /Type /Annot /Subtype /Highlight /Rect [50 50 150 70] /QuadPoints [50 70 150 70 50 50 150 50] /NM (hl) /Contents (Look) /Popup 6 0 R >>",
        b"<< /Type /Annot /Subtype /Popup /Rect [(a) (b) (c) (d)] /Open /Yes /Parent 5 0 R >>",
    ])
}

fn workdir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pdfcraft-popups-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("doc.pdf"), doc_pdf()).unwrap();
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

fn popup_of(a: &mut Automation, doc: u64, id: &str) -> Value {
    let list = ok(a, "comment_list", json!({ "doc": doc }));
    list["comments"].as_array().unwrap().iter().find(|c| c["id"] == id).unwrap_or_else(|| panic!("no {id} in {list}"))["popup"].clone()
}

#[test]
fn pop_ups_open_move_undo_and_survive_a_save() {
    let dir = workdir("main");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "doc.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 1, "type": "note", "at": [20, 20], "contents": "Fix this" }));
    let id = ok(&mut a, "comment_list", json!({ "doc": doc, "page": 1 }))["comments"][0]["id"].as_str().unwrap().to_owned();
    let first = popup_of(&mut a, doc, &id);
    assert_eq!(first["open"], false, "{first}");
    // Next to the note, top edges aligned, 200 × 120.
    assert_eq!(first["rect"], json!([50.0, 20.0, 250.0, 140.0]));

    ok(&mut a, "comment_popup", json!({ "doc": doc, "id": id, "open": true }));
    assert_eq!(popup_of(&mut a, doc, &id)["open"], true);
    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    assert_eq!(popup_of(&mut a, doc, &id)["open"], false, "opening is one undo step");

    let out = ok(&mut a, "comment_popup", json!({ "doc": doc, "id": id, "open": true, "rect": [100, 30, 320, 190] }));
    assert_eq!(out["popup"], json!({ "open": true, "rect": [100.0, 30.0, 320.0, 190.0] }));
    let moved = ok(&mut a, "comment_popup", json!({ "doc": doc, "id": id, "move": [10, -5] }));
    assert_eq!(moved["popup"]["rect"], json!([110.0, 25.0, 330.0, 185.0]));
    // Moving the note takes its pop-up along.
    ok(&mut a, "comment_edit", json!({ "doc": doc, "id": id, "move": [5, 5] }));
    assert_eq!(popup_of(&mut a, doc, &id)["rect"], json!([115.0, 30.0, 335.0, 190.0]));

    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "out.pdf" }));
    let re = ok(&mut a, "doc_open", json!({ "path": "out.pdf" }))["doc"].as_u64().unwrap();
    assert_eq!(popup_of(&mut a, re, &id), json!({ "open": true, "rect": [115.0, 30.0, 335.0, 190.0] }), "kept through save and reopen");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rotated_pages_broken_pop_ups_and_hostile_values() {
    let dir = workdir("hostile");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "doc.pdf" }))["doc"].as_u64().unwrap();
    // The broken pop-up is listed in its default place, closed.
    let broken = popup_of(&mut a, doc, "hl");
    assert_eq!(broken["open"], false, "{broken}");
    assert!(broken["rect"].as_array().unwrap().iter().all(|v| v.as_f64().is_some_and(f64::is_finite)));
    // On a rotated page the rectangle given is the rectangle reported.
    let out = ok(&mut a, "comment_popup", json!({ "doc": doc, "id": "hl", "open": true, "rect": [20, 40, 220, 160] }));
    assert_eq!(out["popup"], json!({ "open": true, "rect": [20.0, 40.0, 220.0, 160.0] }));

    for bad in [
        json!({ "rect": [0, 0, 5, 5] }),
        json!({ "rect": [0, 0, 5000, 100] }),
        json!({ "rect": [0, 0, 1e300, 100] }),
        json!({}),
        json!({ "move": [1e300, 0] }),
    ] {
        let mut args = bad.clone();
        args["doc"] = json!(doc);
        args["id"] = json!("hl");
        assert!(a.call("comment_popup", &args).is_err(), "{bad} must be refused");
    }
    assert!(a.call("comment_popup", &json!({ "doc": doc, "id": "hl", "rect": [0, 0, 100, 100], "move": [1, 1] })).is_err());
    assert_eq!(popup_of(&mut a, doc, "hl")["rect"], json!([20.0, 40.0, 220.0, 160.0]), "refused calls change nothing");

    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 1, "type": "textbox", "rect": [10, 10, 100, 40], "contents": "Box" }));
    assert!(a.call("comment_popup", &json!({ "doc": doc, "page": 1, "index": 1, "open": true })).is_err(), "text boxes have no pop-up");
    ok(&mut a, "comment_reply", json!({ "doc": doc, "id": "hl", "text": "Agreed" }));

    // Open All / Close All Pop-ups.
    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 1, "type": "note", "at": [300, 300] }));
    let opened = ok(&mut a, "comment_popups_all", json!({ "doc": doc }));
    assert_eq!(opened["changed"], 1, "{opened}");
    let closed = ok(&mut a, "comment_popups_all", json!({ "doc": doc, "open": false }));
    assert_eq!(closed["changed"], 2, "{closed}");
    assert_eq!(ok(&mut a, "comment_popups_all", json!({ "doc": doc, "open": false }))["changed"], 0);
    assert_eq!(ok(&mut a, "comment_popups_all", json!({ "doc": doc, "page": 2 }))["changed"], 1);
    assert!(a.call("comment_popups_all", &json!({ "doc": doc, "page": 9 })).is_err());
    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    assert_eq!(popup_of(&mut a, doc, "hl")["open"], false, "Open All is one undo step");
    let _ = std::fs::remove_dir_all(&dir);
}
