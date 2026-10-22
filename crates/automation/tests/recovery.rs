//! Crash recovery headlessly: `recovery_snapshot` writes entries like the app's autosave,
//! `recovery_list` shows only complete ones, `recovery_restore` reopens one unsaved at its
//! original path, `recovery_discard` removes them; damaged entries open nothing; paths stay in
//! the root.

use pdfcraft_automation::{Automation, Content};
use serde_json::{Value, json};

fn pdf(pages: usize) -> Vec<u8> {
    let mut objs = vec!["<< /Type /Catalog /Pages 2 0 R >>".to_string()];
    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", 3 + i)).collect();
    objs.push(format!("<< /Type /Pages /Kids [{}] /Count {pages} >>", kids.join(" ")));
    for _ in 0..pages {
        objs.push("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 400] >>".into());
    }
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
        Ok(_) => panic!("{tool} {args} should fail"),
        Err(e) => e.to_string(),
    }
}

fn root(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("pdfcraft-recovery-tools-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("doc.pdf"), pdf(3)).unwrap();
    dir
}

#[test]
fn snapshot_list_restore_and_discard_through_tools() {
    let dir = root("cycle");
    let mut a = Automation::new().with_root(&dir).unwrap().with_clock(|| 1_700_000_000);
    let doc = ok(&mut a, "doc_open", json!({ "path": "doc.pdf" }))["doc"].clone();
    assert_eq!(ok(&mut a, "recovery_snapshot", json!({ "dir": "rec" }))["written"], json!([]), "nothing changed yet");
    ok(&mut a, "page_delete", json!({ "doc": doc, "pages": [1] }));
    let written = ok(&mut a, "recovery_snapshot", json!({ "dir": "rec" }));
    let key = written["written"][0]["key"].as_str().unwrap().to_string();
    assert_eq!(ok(&mut a, "recovery_snapshot", json!({ "dir": "rec" }))["written"], json!([]), "nothing new");
    // A later change replaces the same entry.
    ok(&mut a, "page_delete", json!({ "doc": doc, "pages": [1] }));
    assert_eq!(ok(&mut a, "recovery_snapshot", json!({ "dir": "rec" }))["written"][0]["key"], json!(key));

    let list = ok(&mut a, "recovery_list", json!({ "dir": "rec" }));
    assert_eq!(list["entries"].as_array().unwrap().len(), 1, "{list}");
    assert_eq!(list["entries"][0]["name"], "doc.pdf");
    assert_eq!(list["entries"][0]["saved_at"], 1_700_000_000);
    assert_eq!(list["damaged"], json!([]));

    // A new session after a crash.
    let mut b = Automation::new().with_root(&dir).unwrap();
    let restored = ok(&mut b, "recovery_restore", json!({ "dir": "rec", "key": key }));
    assert_eq!(restored["pages"], 1, "both edits survived");
    assert_eq!(restored["dirty"], true, "{restored}");
    let original = dir.canonicalize().unwrap().join("doc.pdf");
    assert_eq!(restored["path"], json!(original.to_string_lossy()), "Save goes back to the original file");

    let gone = ok(&mut b, "recovery_discard", json!({ "dir": "rec", "key": key }));
    assert_eq!(gone["discarded"], json!([key]));
    assert_eq!(ok(&mut b, "recovery_list", json!({ "dir": "rec" }))["entries"], json!([]));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn damaged_entries_open_nothing_and_hostile_arguments_are_refused() {
    let dir = root("hostile");
    let mut a = Automation::new().with_root(&dir).unwrap();
    let doc = ok(&mut a, "doc_open", json!({ "path": "doc.pdf" }))["doc"].clone();
    ok(&mut a, "page_delete", json!({ "doc": doc, "pages": [1] }));
    let key = ok(&mut a, "recovery_snapshot", json!({ "dir": "rec" }))["written"][0]["key"].as_str().unwrap().to_string();
    let rec = dir.join("rec");
    let pdf_path = rec.join(format!("{key}.pdf"));
    let good = std::fs::read(&pdf_path).unwrap();

    // Same length, one byte changed: listed, but restoring refuses.
    let mut flipped = good.clone();
    let mid = flipped.len() / 2;
    flipped[mid] ^= 1;
    std::fs::write(&pdf_path, &flipped).unwrap();
    let mut b = Automation::new().with_root(&dir).unwrap();
    assert!(err(&mut b, "recovery_restore", json!({ "dir": "rec", "key": key })).contains("damaged"));
    assert!(b.session().docs().is_empty(), "nothing was opened");

    // Truncated: not listed at all, and named as damaged.
    std::fs::write(&pdf_path, &good[..good.len() - 7]).unwrap();
    std::fs::write(rec.join("junk.json"), b"\xff\xfe not json").unwrap();
    std::fs::write(rec.join("lonely.pdf"), b"%PDF-1.7").unwrap();
    std::fs::write(rec.join("readme.txt"), b"keep").unwrap();
    let list = ok(&mut b, "recovery_list", json!({ "dir": "rec" }));
    assert_eq!(list["entries"], json!([]), "{list}");
    assert_eq!(list["damaged"].as_array().unwrap().len(), 4, "{list}");
    assert!(err(&mut b, "recovery_restore", json!({ "dir": "rec", "key": key })).contains("no complete recovery entry"));

    // Keys that would name other files, and paths outside the root.
    for k in ["../doc", "a/b", "", "x.pdf"] {
        assert!(b.call("recovery_restore", &json!({ "dir": "rec", "key": k })).is_err(), "{k:?}");
        assert!(b.call("recovery_discard", &json!({ "dir": "rec", "key": k })).is_err(), "{k:?}");
    }
    assert!(err(&mut b, "recovery_list", json!({ "dir": "../.." })).contains("outside"));
    assert!(err(&mut b, "recovery_snapshot", json!({ "dir": "/" })).contains("outside"));
    assert!(err(&mut b, "recovery_list", json!({ "dir": "doc.pdf" })).contains("not a folder"));
    assert!(err(&mut b, "recovery_discard", json!({ "dir": "rec" })).contains("key"));
    assert!(err(&mut b, "recovery_discard", json!({ "dir": "rec", "key": "k", "all": true })).contains("not both"));

    // all removes every entry and the damaged leftovers, and nothing else.
    let out = ok(&mut b, "recovery_discard", json!({ "dir": "rec", "all": true }));
    assert_eq!(out["damaged_removed"].as_array().unwrap().len(), 4, "{out}");
    let left: Vec<String> = std::fs::read_dir(&rec).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(left, ["readme.txt"]);
    let _ = std::fs::remove_dir_all(&dir);
}
