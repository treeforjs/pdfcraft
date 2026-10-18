//! `search_folder` end to end, on temp folders of synthetic PDFs.

use std::path::{Path, PathBuf};

use pdfcraft_automation::{Automation, Content, ToolError};
use serde_json::{Value, json};

/// A PDF whose page i reads `pages[i]` (no parentheses or backslashes).
fn pdf(pages: &[&str]) -> Vec<u8> {
    let n = pages.len();
    let mut objs: Vec<Vec<u8>> = vec![b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()];
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
    objs.push(format!("<< /Type /Pages /Kids [{}] /Count {n} /MediaBox [0 0 400 300] >>", kids.join(" ")).into_bytes());
    objs.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec());
    for (i, text) in pages.iter().enumerate() {
        objs.push(format!("<< /Type /Page /Parent 2 0 R /Contents {} 0 R /Resources << /Font << /F1 3 0 R >> >> >>", 5 + 2 * i).into_bytes());
        let body = format!("BT /F1 18 Tf 20 150 Td ({text}) Tj ET");
        objs.push(format!("<< /Length {} >>\nstream\n{body}\nendstream", body.len()).into_bytes());
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
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

fn dir(test: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pdfcraft-search-folder-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write(root: &Path, rel: &str, bytes: &[u8]) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, bytes).unwrap();
}

/// docs/ holds a.pdf (2 pages), b.pdf (1 page), notes.txt and sub/deep/c.pdf.
fn corpus(test: &str) -> (PathBuf, Automation) {
    let root = dir(test);
    write(&root, "docs/a.pdf", &pdf(&["The fox jumps over the dog", "Foxes are quick, a FOX is red"]));
    write(&root, "docs/b.pdf", &pdf(&["No animals here"]));
    write(&root, "docs/notes.txt", b"fox fox fox");
    write(&root, "docs/sub/deep/c.pdf", &pdf(&["A fox in a subfolder"]));
    let a = Automation::new().with_root(&root).unwrap().with_clock(|| 1_700_000_000);
    (root, a)
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

fn search(a: &mut Automation, args: Value) -> Value {
    ok(a, "search_folder", args)
}

fn file<'a>(r: &'a Value, path: &str) -> Option<&'a Value> {
    r["results"].as_array().unwrap().iter().find(|f| f["path"].as_str().unwrap().replace('\\', "/") == path)
}

fn pages_of(f: &Value) -> Vec<u64> {
    f["matches"].as_array().unwrap().iter().map(|m| m["page"].as_u64().unwrap()).collect()
}

#[test]
fn matches_across_files_and_pages_like_text_find() {
    let (root, mut a) = corpus("basic");
    let r = search(&mut a, json!({ "folder": "docs", "query": "fox" }));
    assert_eq!(r["files_found"], 2, "not recursive: a.pdf and b.pdf only; {r}");
    assert_eq!(r["files_searched"], 2);
    assert_eq!(r["files_matched"], 1);
    assert_eq!(r["truncated"], false);
    let f = file(&r, "docs/a.pdf").expect("a.pdf matched");
    // "fox" on page 1, "Fox"(es) and "FOX" on page 2.
    assert_eq!(pages_of(f), vec![1, 2, 2], "{f}");
    assert_eq!(r["total_matches"], 3);
    let m = &f["matches"][0];
    assert_eq!(m["found_in"], "text");
    assert_eq!(m["text"], "fox");
    assert_eq!(m["before"], "The");
    assert_eq!(m["after"], "jumps over the dog");
    assert!(file(&r, "docs/b.pdf").is_none(), "no matches, not listed");
    assert!(r["skipped"].as_array().unwrap().is_empty(), "{r}");

    // The page-text matches are exactly text_find's for the same file.
    let doc = ok(&mut a, "doc_open", json!({ "path": "docs/a.pdf" }))["doc"].as_u64().unwrap();
    let tf = ok(&mut a, "text_find", json!({ "doc": doc, "query": "fox" }));
    let theirs: Vec<(Value, Value, Value)> =
        tf["matches"].as_array().unwrap().iter().map(|m| (m["page"].clone(), m["text"].clone(), m["rects"].clone())).collect();
    let ours: Vec<(Value, Value, Value)> =
        f["matches"].as_array().unwrap().iter().map(|m| (m["page"].clone(), m["text"].clone(), m["rects"].clone())).collect();
    assert_eq!(ours, theirs);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn whole_words_and_case_options() {
    let (root, mut a) = corpus("options");
    let whole = search(&mut a, json!({ "folder": "docs", "query": "fox", "whole_words": true }));
    // "Foxes" is not the whole word.
    assert_eq!(pages_of(file(&whole, "docs/a.pdf").unwrap()), vec![1, 2]);
    let case = search(&mut a, json!({ "folder": "docs", "query": "FOX", "case_sensitive": true }));
    let f = file(&case, "docs/a.pdf").unwrap();
    assert_eq!(pages_of(f), vec![2]);
    assert_eq!(f["matches"][0]["text"], "FOX");
    let both = search(&mut a, json!({ "folder": "docs", "query": "Fox", "case_sensitive": true, "whole_words": true }));
    assert_eq!(both["total_matches"], 0, "only Foxes has that case; {both}");
    assert!(both["results"].as_array().unwrap().is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn subfolders_only_when_recursive() {
    let (root, mut a) = corpus("recursive");
    let flat = search(&mut a, json!({ "folder": "docs", "query": "subfolder" }));
    assert_eq!(flat["total_matches"], 0);
    let deep = search(&mut a, json!({ "folder": "docs", "query": "subfolder", "recursive": true }));
    assert_eq!(deep["files_found"], 3);
    let f = file(&deep, "docs/sub/deep/c.pdf").expect("found in the subfolder");
    assert_eq!(pages_of(f), vec![1]);
    // Absolute folder paths inside the root work too, and paths are reported under them.
    let abs = root.join("docs").canonicalize().unwrap();
    let r = search(&mut a, json!({ "folder": abs.to_string_lossy(), "query": "subfolder", "recursive": true }));
    assert_eq!(r["total_matches"], 1);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn bookmarks_comments_and_attachments_on_request() {
    use base64::Engine as _;
    let (root, mut a) = corpus("extras");
    let doc = ok(&mut a, "doc_open", json!({ "path": "docs/b.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "bookmark_add", json!({ "doc": doc, "title": "Chapter on the Fox", "page": 1 }));
    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 1, "type": "note", "at": [20, 20], "contents": "check this fox" }));
    let inner = base64::engine::general_purpose::STANDARD.encode(pdf(&["an attached fox page"]));
    ok(&mut a, "associated_file_add", json!({ "doc": doc, "data_base64": inner, "name": "fox-appendix.pdf" }));
    ok(&mut a, "doc_save", json!({ "doc": doc }));
    ok(&mut a, "doc_close", json!({ "doc": doc }));

    let plain = search(&mut a, json!({ "folder": "docs", "query": "fox" }));
    assert!(file(&plain, "docs/b.pdf").is_none(), "off by default; {plain}");
    let r =
        search(&mut a, json!({ "folder": "docs", "query": "fox", "include_bookmarks": true, "include_comments": true, "include_attachments": true }));
    let f = file(&r, "docs/b.pdf").expect("b.pdf matched");
    let kinds: Vec<&str> = f["matches"].as_array().unwrap().iter().map(|m| m["found_in"].as_str().unwrap()).collect();
    assert_eq!(kinds, vec!["bookmark", "comment", "attachment", "attachment_text"], "{f}");
    let m = &f["matches"];
    assert_eq!(m[0]["text"], "Fox");
    assert_eq!(m[0]["before"], "Chapter on the");
    assert_eq!(m[0]["bookmark"], json!([1]));
    assert_eq!(m[0]["page"], 1);
    assert_eq!(m[1]["page"], 1);
    assert_eq!(m[1]["subtype"], "Text");
    assert_eq!(m[1]["before"], "check this");
    assert_eq!(m[2]["attachment"], "fox-appendix.pdf");
    assert_eq!(m[2]["field"], "name");
    assert_eq!(m[3]["attachment"], "fox-appendix.pdf");
    assert_eq!(m[3]["text"], "fox");
    assert_eq!(m[3]["page"], 1);
    // Options apply to them too.
    let case =
        search(&mut a, json!({ "folder": "docs", "query": "Fox", "case_sensitive": true, "include_bookmarks": true, "include_comments": true }));
    let kinds: Vec<&str> =
        file(&case, "docs/b.pdf").unwrap()["matches"].as_array().unwrap().iter().map(|m| m["found_in"].as_str().unwrap()).collect();
    assert_eq!(kinds, vec!["bookmark"]);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn corrupt_and_encrypted_files_are_skipped_with_reasons() {
    let (root, mut a) = corpus("skipped");
    write(&root, "docs/broken.pdf", b"%PDF-1.7\nthis is not really a PDF at all");
    write(&root, "docs/empty.pdf", b"");
    let doc = ok(&mut a, "doc_open", json!({ "path": "docs/b.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "doc_protect", json!({ "doc": doc, "open_password": "s3cret" }));
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "docs/locked.pdf" }));
    ok(&mut a, "doc_close", json!({ "doc": doc, "discard_changes": true }));

    let r = search(&mut a, json!({ "folder": "docs", "query": "animals" }));
    let reason = |name: &str| -> String {
        let s = r["skipped"].as_array().unwrap().iter().find(|s| s["path"].as_str().unwrap().ends_with(name));
        s.unwrap_or_else(|| panic!("{name} not skipped: {r}"))["reason"].as_str().unwrap().to_string()
    };
    assert!(reason("locked.pdf").contains("password"), "{r}");
    assert!(!reason("broken.pdf").is_empty());
    assert!(!reason("empty.pdf").is_empty());
    // The good files are still searched.
    assert_eq!(pages_of(file(&r, "docs/b.pdf").unwrap()), vec![1]);
    assert_eq!(r["files_searched"], 2);

    // With the password, the protected file is searched; the others still open without it.
    let r = search(&mut a, json!({ "folder": "docs", "query": "animals", "password": "s3cret" }));
    assert!(file(&r, "docs/locked.pdf").is_some(), "{r}");
    assert!(file(&r, "docs/b.pdf").is_some(), "{r}");
    assert!(!r.to_string().contains("s3cret"), "the password is never echoed");
    let wrong = search(&mut a, json!({ "folder": "docs", "query": "animals", "password": "nope" }));
    let s = wrong["skipped"].as_array().unwrap().iter().find(|s| s["path"].as_str().unwrap().ends_with("locked.pdf")).unwrap().clone();
    assert!(s["reason"].as_str().unwrap().contains("does not open it"), "{s}");
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn symlink_loops_and_links_out_of_the_root_are_not_followed() {
    let (root, mut a) = corpus("links");
    // A loop back to the folder itself, and one to its parent.
    std::os::unix::fs::symlink(root.join("docs"), root.join("docs/sub/loop")).unwrap();
    std::os::unix::fs::symlink("..", root.join("docs/up")).unwrap();
    // A link to a PDF outside the root, and one to a PDF inside it.
    let outside = dir("links-outside");
    write(&outside, "secret.pdf", &pdf(&["fox outside"]));
    std::os::unix::fs::symlink(outside.join("secret.pdf"), root.join("docs/secret.pdf")).unwrap();
    std::os::unix::fs::symlink(root.join("docs/sub/deep/c.pdf"), root.join("docs/alias.pdf")).unwrap();
    let r = search(&mut a, json!({ "folder": "docs", "query": "fox", "recursive": true }));
    let skipped = r["skipped"].to_string();
    assert!(skipped.contains("docs/sub/loop") && skipped.contains("not followed"), "{r}");
    assert!(skipped.contains("docs/up"), "{r}");
    assert!(skipped.contains("docs/secret.pdf") && skipped.contains("outside the allowed directory"), "{r}");
    assert!(!r.to_string().contains("fox outside"));
    // a.pdf, and c.pdf once (through the link, which sorts first), never twice.
    assert_eq!(r["files_found"], 3, "{r}");
    let paths: Vec<String> = r["results"].as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap().to_string()).collect();
    assert_eq!(paths, vec!["docs/a.pdf", "docs/alias.pdf"], "{r}");
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(outside).unwrap();
}

#[test]
fn caps_truncate_and_say_so() {
    let (root, mut a) = corpus("caps");
    let few = search(&mut a, json!({ "folder": "docs", "query": "fox", "recursive": true, "max_files": 1 }));
    assert_eq!(few["files_found"], 1);
    assert_eq!(few["truncated"], true);
    assert_eq!(few["truncated_by"], json!(["max_files"]));
    let exact = search(&mut a, json!({ "folder": "docs", "query": "fox", "max_files": 2 }));
    assert_eq!(exact["truncated"], false, "exactly two PDFs: nothing left out; {exact}");

    let two = search(&mut a, json!({ "folder": "docs", "query": "fox", "recursive": true, "max_results": 2 }));
    assert_eq!(two["total_matches"], 2);
    assert_eq!(pages_of(file(&two, "docs/a.pdf").unwrap()), vec![1, 2], "the first matches in page order");
    assert_eq!(two["truncated"], true);
    assert_eq!(two["truncated_by"], json!(["max_results"]));
    let three = search(&mut a, json!({ "folder": "docs", "query": "fox", "recursive": true, "max_results": 3 }));
    assert_eq!(three["total_matches"], 3);
    assert_eq!(three["files_not_searched"], 2, "b.pdf and c.pdf were not reached; {three}");
    assert_eq!(three["truncated_by"], json!(["max_results"]));

    for bad in [
        json!({ "folder": "docs", "query": "fox", "max_files": 0 }),
        json!({ "folder": "docs", "query": "fox", "max_files": 10001 }),
        json!({ "folder": "docs", "query": "fox", "max_results": -1 }),
        json!({ "folder": "docs", "query": "   " }),
        json!({ "folder": "docs", "query": "x".repeat(1001) }),
        json!({ "folder": "docs", "query": "fox", "recursive": "yes" }),
        json!({ "folder": "docs", "query": "fox", "deep": true }),
        json!({ "folder": "docs" }),
    ] {
        assert!(matches!(a.call("search_folder", &bad), Err(ToolError::InvalidArgs(_))), "{bad}");
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn the_folder_must_be_inside_the_root() {
    let (root, mut a) = corpus("confined");
    let outside = dir("confined-outside");
    write(&outside, "x.pdf", &pdf(&["fox"]));
    let refused = |a: &mut Automation, folder: &str| match a.call("search_folder", &json!({ "folder": folder, "query": "fox" })) {
        Err(ToolError::Failed(m)) => m,
        other => panic!("{folder}: expected a refusal, got {other:?}"),
    };
    // Outside, by absolute path, by .. and through a link: the same refusal, existing or not.
    let m = refused(&mut a, &outside.to_string_lossy());
    assert!(m.contains("outside the allowed directory"), "{m}");
    assert!(refused(&mut a, "../").contains("outside the allowed directory"));
    assert!(refused(&mut a, "docs/../../nowhere").contains("outside the allowed directory"));
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside, root.join("out")).unwrap();
        assert!(refused(&mut a, "out").contains("outside the allowed directory"));
    }
    // Inside: a file is not a folder, and a missing folder is reported as missing.
    assert!(refused(&mut a, "docs/a.pdf").contains("not a folder"));
    let m = refused(&mut a, "docs/missing");
    assert!(!m.contains("outside"), "{m}");
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(outside).unwrap();
}
