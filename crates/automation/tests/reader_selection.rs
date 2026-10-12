//! Headless reader selections use original synthetic documents and public engine APIs.
use pdfcraft_automation::{Automation, Content};
use serde_json::{Value, json};
fn call(a: &mut Automation, tool: &str, args: Value) -> Value {
    let result = a.call(tool, &args).unwrap();
    match result.first().unwrap() {
        Content::Json(v) => v.clone(),
        _ => panic!("expected JSON"),
    }
}
fn pdf() -> Vec<u8> {
    let content = "BT /F1 14 Tf 20 150 Td (First row) Tj 0 -18 Td (Next row) Tj ET";
    format!("%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >> endobj\n3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 5 0 R /Resources << /Font << /F1 6 0 R >> >> >> endobj\n4 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 5 0 R /Resources << /Font << /F1 6 0 R >> >> >> endobj\n5 0 obj << /Length {} >> stream\n{content}\nendstream endobj\n6 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj\ntrailer << /Root 1 0 R >>\n%%EOF",content.len()).into_bytes()
}
fn setup(name: &str) -> (Automation, std::path::PathBuf, u64) {
    let root = std::env::temp_dir().join(format!("pdfcraft-reader-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("reader.pdf"), pdf()).unwrap();
    let mut a = Automation::new().with_root(&root).unwrap();
    let id = call(&mut a, "doc_open", json!({"path":"reader.pdf"}))["doc"].as_u64().unwrap();
    (a, root, id)
}
#[test]
fn reader_range_paragraph_and_all_are_read_only_and_validate_every_endpoint() {
    let (mut a, root, id) = setup("ranges");
    let before = call(&mut a, "doc_info", json!({"doc":id}));
    let glyphs = call(&mut a, "text_extract", json!({"doc":id,"pages":[1],"include_glyphs":true}));
    assert_eq!(glyphs["pages"][0]["glyphs"][0]["glyph"], 1);
    assert_eq!(glyphs["pages"][0]["glyphs"][0]["text"], "F");
    let all = call(&mut a, "text_selection", json!({"doc":id,"mode":"all"}));
    assert_eq!(all["text"], "First row\nNext row\n\nFirst row\nNext row");
    let paragraph = call(&mut a, "text_selection", json!({"doc":id,"mode":"paragraph","start_page":1,"start_glyph":4}));
    assert_eq!(paragraph["text"], "First row\nNext row");
    let range = call(&mut a, "text_selection", json!({"doc":id,"start_page":2,"start_glyph":5,"end_page":1,"end_glyph":7}));
    assert_eq!(range["text"], "row\nNext row\n\nFirst");
    for invalid in [
        json!({"mode":"all","start_page":1}),
        json!({"start_page":0,"start_glyph":1,"end_page":2,"end_glyph":3}),
        json!({"start_page":1,"start_glyph":1,"end_page":3,"end_glyph":1}),
        json!({"start_page":1,"start_glyph":1,"end_page":2,"end_glyph":9223372036854775807i64}),
        json!({"mode":"paragraph","start_page":1,"start_glyph":1,"end_page":1}),
        json!({"mode":"unknown"}),
    ] {
        let mut invalid = invalid;
        invalid["doc"] = id.into();
        assert!(a.call("text_selection", &invalid).is_err());
    }
    assert_eq!(call(&mut a, "doc_info", json!({"doc":id})), before);
    let _ = std::fs::remove_dir_all(root);
}
#[test]
fn restricted_copy_and_legacy_extract_refuse_but_owner_can_copy() {
    let (mut a, root, id) = setup("permissions");
    call(&mut a, "doc_protect", json!({"doc":id,"permissions_password":"Original-owner-test","copy":false}));
    call(&mut a, "doc_save", json!({"doc":id,"path":"restricted.pdf"}));
    let user = call(&mut a, "doc_open", json!({"path":"restricted.pdf"}))["doc"].as_u64().unwrap();
    for (tool, args) in [("text_selection", json!({"doc":user,"mode":"all"})), ("text_extract", json!({"doc":user}))] {
        assert!(a.call(tool, &args).unwrap_err().to_string().contains("does not allow copying"));
    }
    let owner = call(&mut a, "doc_open", json!({"path":"restricted.pdf","password":"Original-owner-test"}))["doc"].as_u64().unwrap();
    assert!(call(&mut a, "text_selection", json!({"doc":owner,"mode":"all"}))["text"].as_str().unwrap().contains("First row"));
    let _ = std::fs::remove_dir_all(root);
}
#[test]
fn encrypted_copy_denied_refuses_every_reader_path_without_leaking_text() {
    let (mut a, root, id) = setup("encrypted");
    call(
        &mut a,
        "doc_protect",
        json!({"doc":id,"open_password":"Reader-user-test","permissions_password":"Reader-owner-test","copy":false,"accessibility":true}),
    );
    call(&mut a, "doc_save", json!({"doc":id,"path":"encrypted.pdf"}));
    let bytes = std::fs::read(root.join("encrypted.pdf")).unwrap();
    assert!(bytes.windows(9).all(|w| w != b"First row"), "content must be encrypted on disk");
    assert!(a.call("doc_open", &json!({"path":"encrypted.pdf"})).is_err(), "open password is required");
    let user = call(&mut a, "doc_open", json!({"path":"encrypted.pdf","password":"Reader-user-test"}))["doc"].as_u64().unwrap();
    let before = call(&mut a, "doc_info", json!({"doc":user}));
    for (tool, args) in [
        ("text_selection", json!({"doc":user,"mode":"all"})),
        ("text_selection", json!({"doc":user,"start_page":1,"start_glyph":1,"end_page":2,"end_glyph":3})),
        ("text_selection", json!({"doc":user,"mode":"paragraph","start_page":1,"start_glyph":1})),
        ("text_extract", json!({"doc":user})),
        ("text_extract", json!({"doc":user,"pages":[2],"include_glyphs":true})),
    ] {
        let error = a.call(tool, &args).unwrap_err().to_string();
        assert!(error.contains("does not allow copying"), "{tool} {args}: {error}");
        for leaked in ["First", "row", "Next"] {
            assert!(!error.contains(leaked), "{tool} error leaked document text: {error}");
        }
    }
    // Accessibility permission does not grant clipboard copying; refusal changes nothing.
    assert_eq!(call(&mut a, "doc_info", json!({"doc":user})), before);
    let owner = call(&mut a, "doc_open", json!({"path":"encrypted.pdf","password":"Reader-owner-test"}))["doc"].as_u64().unwrap();
    assert_eq!(call(&mut a, "text_selection", json!({"doc":owner,"mode":"all"}))["text"], "First row\nNext row\n\nFirst row\nNext row");
    let glyphs = call(&mut a, "text_extract", json!({"doc":owner,"pages":[2],"include_glyphs":true}));
    assert_eq!(glyphs["pages"][0]["glyphs"][0]["text"], "F");
    let _ = std::fs::remove_dir_all(root);
}
