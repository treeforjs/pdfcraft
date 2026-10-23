//! End to end: per-type comment hiding, snapped carets, typed page ranges, batch runs over a
//! folder with subfolders, and the session tool host custom commands run through, with hostile
//! arguments throughout.

use std::path::{Path, PathBuf};

use pdfcraft_automation::{Automation, Content, SessionTools};
use pdfcraft_engine::tool_host::ToolHost;
use serde_json::{Value, json};

/// A PDF whose page i reads `pages[i]`; page 2 (when there is one) is landscape and page 1
/// carries a sticky note and a square.
fn pdf(pages: &[&str]) -> Vec<u8> {
    let n = pages.len();
    let first = 4 + 2 * n;
    let mut objs: Vec<Vec<u8>> = vec![b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()];
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
    objs.push(format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")).into_bytes());
    objs.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec());
    for (i, text) in pages.iter().enumerate() {
        let media = if i == 1 { "[0 0 600 400]" } else { "[0 0 400 600]" };
        let annots = if i == 0 { format!(" /Annots [{first} 0 R {} 0 R]", first + 1) } else { String::new() };
        objs.push(
            format!("<< /Type /Page /Parent 2 0 R /MediaBox {media} /Contents {} 0 R /Resources << /Font << /F1 3 0 R >> >>{annots} >>", 5 + 2 * i)
                .into_bytes(),
        );
        let body = format!("BT /F1 20 Tf 40 300 Td ({text}) Tj ET");
        objs.push(format!("<< /Length {} >>\nstream\n{body}\nendstream", body.len()).into_bytes());
    }
    objs.push(b"<< /Type /Annot /Subtype /Text /Rect [10 10 30 30] /Contents (note) >>".to_vec());
    objs.push(b"<< /Type /Annot /Subtype /Square /Rect [50 50 90 90] /Contents (box) >>".to_vec());
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

fn dir(test: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pdfcraft-ui-gaps-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write(root: &Path, rel: &str, bytes: &[u8]) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, bytes).unwrap();
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

fn open(test: &str, pages: &[&str]) -> (PathBuf, Automation, u64) {
    let root = dir(test);
    write(&root, "doc.pdf", &pdf(pages));
    let mut a = Automation::new().with_root(&root).unwrap().with_clock(|| 1_700_000_000);
    let doc = ok(&mut a, "doc_open", json!({ "path": "doc.pdf" }))["doc"].as_u64().unwrap();
    (root, a, doc)
}

#[test]
fn comments_hide_by_type_and_refuse_odd_types() {
    let (_root, mut a, doc) = open("hide", &["Hello world"]);
    let r = ok(&mut a, "comments_hide", json!({ "doc": doc, "types": ["Square"] }));
    assert_eq!(r["hidden_types"], json!(["Square"]));
    assert_eq!(r["hidden"], false);
    let r = ok(&mut a, "comments_hide", json!({ "doc": doc, "types": ["Text", "Square", "Text"] }));
    assert_eq!(r["hidden_types"], json!(["Square", "Text"]));
    let r = ok(&mut a, "comments_hide", json!({ "doc": doc, "types": [] }));
    assert_eq!(r["hidden_types"], json!([]));
    for bad in [
        json!({ "doc": doc, "types": ["Widget"] }),
        json!({ "doc": doc, "types": ["Link"] }),
        json!({ "doc": doc, "types": ["../x"] }),
        json!({ "doc": doc, "types": ["x".repeat(500)] }),
        json!({ "doc": doc, "types": "Square" }),
        json!({ "doc": doc, "types": ["Square"], "hidden": true }),
        json!({ "doc": doc, "types": (0..200).map(|i| format!("T{i}")).collect::<Vec<_>>() }),
    ] {
        assert!(a.call("comments_hide", &bad).is_err(), "{bad}");
    }
    // A refused call leaves the setting as it was.
    let r = ok(&mut a, "comments_hide", json!({ "doc": doc, "hidden": false }));
    assert_eq!(r["hidden_types"], json!([]));
}

#[test]
fn caret_snaps_between_characters() {
    let (_root, mut a, doc) = open("caret", &["Hello world"]);
    // Text starts at x = 40 on the baseline y = 300 (user space, page 400 × 600; `at` is top-down).
    let r = ok(&mut a, "comment_add", json!({ "doc": doc, "page": 1, "type": "caret", "at": [93.0, 296.0], "snap": true }));
    let list = ok(&mut a, "comment_list", json!({ "doc": doc }));
    let carets: Vec<&Value> = list["comments"].as_array().unwrap().iter().filter(|c| c["type"] == "Caret").collect();
    assert_eq!(carets.len(), 1, "{r} {list}");
    // Far from any text, snapping refuses rather than guessing; non-finite points are refused.
    assert!(a.call("comment_add", &json!({ "doc": doc, "page": 1, "type": "caret", "at": [390.0, 20.0], "snap": true })).is_err());
    assert!(a.call("comment_add", &json!({ "doc": doc, "page": 1, "type": "caret", "at": [1e308, -1e308], "snap": true })).is_err());
    assert!(a.call("comment_add", &json!({ "doc": doc, "page": 1, "type": "caret", "at": [93.0, 296.0], "snap": "yes" })).is_err());
    // Without snap it goes where it was asked.
    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 1, "type": "caret", "at": [390.0, 20.0] }));
}

#[test]
fn page_ranges_select_pages() {
    let (_root, mut a, doc) = open("ranges", &["one", "two", "three", "four", "five"]);
    let r = ok(&mut a, "page_range_select", json!({ "doc": doc, "range": "1-3, 5" }));
    assert_eq!(r["pages"], json!([1, 2, 3, 5]));
    let r = ok(&mut a, "page_range_select", json!({ "doc": doc, "range": "", "subset": "even" }));
    assert_eq!(r["pages"], json!([2, 4]));
    let r = ok(&mut a, "page_range_select", json!({ "doc": doc, "orientation": "landscape" }));
    assert_eq!(r["pages"], json!([2]));
    let r = ok(&mut a, "page_range_select", json!({ "doc": doc, "range": "4-" }));
    assert_eq!(r["pages"], json!([4, 5]));
    for bad in [
        json!({ "doc": doc, "range": "0" }),
        json!({ "doc": doc, "range": "9" }),
        json!({ "doc": doc, "range": "3-1-2" }),
        json!({ "doc": doc, "range": "18446744073709551616" }),
        json!({ "doc": doc, "range": "1,".repeat(5000) }),
        json!({ "doc": doc, "range": "x".repeat(10_000) }),
        json!({ "doc": doc, "subset": "prime" }),
        json!({ "doc": doc, "orientation": "square" }),
        json!({ "doc": doc, "range": 5 }),
    ] {
        assert!(a.call("page_range_select", &bad).is_err(), "{bad}");
    }
}

#[test]
fn actions_run_over_a_folder_and_its_subfolders() {
    let root = dir("batch");
    write(&root, "in/a.pdf", &pdf(&["alpha"]));
    write(&root, "in/notes.txt", b"not a pdf");
    write(&root, "in/sub/deep/b.pdf", &pdf(&["beta"]));
    write(&root, "elsewhere/secret.pdf", &pdf(&["secret"]));
    #[cfg(unix)]
    {
        // A link out of the folder is skipped, as is a link to a folder (no cycles).
        std::os::unix::fs::symlink(root.join("elsewhere/secret.pdf"), root.join("in/link.pdf")).unwrap();
        std::os::unix::fs::symlink(root.join("in"), root.join("in/sub/loop")).unwrap();
    }
    let mut a = Automation::new().with_root(&root).unwrap().with_clock(|| 1_700_000_000);
    let r = ok(&mut a, "action_run", json!({ "steps": [{ "step": "set_title", "arg": "T" }], "source_folder": "in", "folder": "out" }));
    assert_eq!(r["files"].as_array().unwrap().len(), 1, "{r}");
    assert!(root.join("out/a.pdf").is_file());
    assert!(!root.join("out/sub").exists());
    let r = ok(
        &mut a,
        "action_run",
        json!({ "steps": [{ "step": "set_title", "arg": "T" }], "source_folder": "in", "recursive": true, "folder": "out2" }),
    );
    assert_eq!(r["files"].as_array().unwrap().len(), 2, "{r}");
    assert!(root.join("out2/a.pdf").is_file());
    assert!(root.join("out2/sub/deep/b.pdf").is_file(), "subfolders are kept in the output");
    assert!(!root.join("out2/link.pdf").exists());
    assert!(r["walk"].is_object());
    for bad in [
        json!({ "action": "add page numbers", "source_folder": "in", "paths": ["in/a.pdf"], "folder": "x" }),
        json!({ "action": "add page numbers", "folder": "x" }),
        json!({ "action": "add page numbers", "source_folder": "../..", "folder": "x" }),
        json!({ "action": "add page numbers", "source_folder": "/", "folder": "x" }),
        json!({ "action": "add page numbers", "source_folder": "in/a.pdf", "folder": "x" }),
        json!({ "action": "add page numbers", "paths": ["in/a.pdf"], "recursive": true, "folder": "x" }),
        json!({ "action": "add page numbers", "source_folder": "in", "recursive": "deep", "folder": "x" }),
    ] {
        assert!(a.call("action_run", &bad).is_err(), "{bad}");
    }
}

#[test]
fn session_tools_run_on_the_callers_session() {
    let (root, mut a, doc) = open("host", &["Hello world", "page two"]);
    let host = SessionTools { root: Some(root.clone()) };
    let tools = host.document_tools();
    assert!(tools.iter().any(|t| t.name == "page_rotate"), "{:?}", tools.iter().map(|t| &t.name).collect::<Vec<_>>());
    assert!(!tools.iter().any(|t| t.name == "doc_open"));
    let session = a.session_mut();
    let r = host.call(session, "page_rotate", &json!({ "doc": doc, "pages": [1], "degrees": 90 }));
    assert!(r.is_ok(), "{r:?}");
    let pages = ok(&mut a, "doc_info", json!({ "doc": doc }));
    assert_eq!(pages["pages"][0]["rotation"], 90, "{pages}");
    // Errors come back as messages and the session (with its document) stays.
    let session = a.session_mut();
    assert!(host.call(session, "page_rotate", &json!({ "doc": doc, "pages": [99], "degrees": 90 })).is_err());
    assert!(host.call(session, "no_such_tool", &json!({})).is_err());
    assert!(host.call(session, "page_rotate", &json!("not an object")).is_err());
    assert!(ok(&mut a, "doc_list", json!({}))["documents"].as_array().unwrap().len() == 1);
}
