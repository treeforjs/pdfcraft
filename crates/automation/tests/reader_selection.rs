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
/// Every other headless path that returns page text: locations-only search, null text fields,
/// refused exports and comparisons on a copy-denied encrypted document; the owner password
/// lifts each. No output or error may contain the document text.
#[test]
fn copy_denied_text_paths_withhold_or_refuse_and_owner_lifts() {
    let (mut a, root, id) = setup("guard");
    call(&mut a, "page_add_text", json!({"doc":id,"page":1,"at":[20,20],"text":"Added secret"}));
    call(
        &mut a,
        "doc_protect",
        json!({"doc":id,"open_password":"Guard-user-test","permissions_password":"Guard-owner-test","copy":false,"changes":"any-except-extract"}),
    );
    call(&mut a, "doc_save", json!({"doc":id,"path":"guard.pdf"}));
    let plain = call(&mut a, "doc_open", json!({"path":"reader.pdf"}))["doc"].as_u64().unwrap();
    let user = call(&mut a, "doc_open", json!({"path":"guard.pdf","password":"Guard-user-test"}))["doc"].as_u64().unwrap();
    let owner = call(&mut a, "doc_open", json!({"path":"guard.pdf","password":"Guard-owner-test"}))["doc"].as_u64().unwrap();
    let no_leak = |what: &str, s: &str| {
        for leaked in ["First", "Next", "secret", "Added"] {
            assert!(!s.contains(leaked), "{what} leaked document text {leaked:?}: {s}");
        }
    };
    let withheld = |what: &str, v: &Value| {
        assert!(v["text_withheld"].as_str().is_some_and(|w| w.contains("does not allow copying")), "{what}: {v}");
        no_leak(what, &v.to_string());
    };

    let found = call(&mut a, "text_find", json!({"doc":user,"query":"first ROW"}));
    assert_eq!(found["count"], 2, "search still finds both pages: {found}");
    for m in found["matches"].as_array().unwrap() {
        assert!(m["text"].is_null() && !m["rects"].as_array().unwrap().is_empty(), "{m}");
    }
    withheld("text_find", &found);
    let lines = call(&mut a, "text_lines", json!({"doc":user,"page":1}));
    assert!(lines["count"].as_u64().unwrap() >= 2 && lines["lines"].as_array().unwrap().iter().all(|l| l["text"].is_null()), "{lines}");
    withheld("text_lines", &lines);
    let paragraphs = call(&mut a, "text_paragraphs", json!({"doc":user,"page":1}));
    assert!(paragraphs["count"].as_u64().unwrap() >= 1 && paragraphs["paragraphs"].as_array().unwrap().iter().all(|p| p["text"].is_null()));
    withheld("text_paragraphs", &paragraphs);
    let content = call(&mut a, "content_list", json!({"doc":user}));
    assert_eq!(content["count"], 1, "{content}");
    assert!(content["items"][0]["text"].is_null());
    withheld("content_list", &content);
    let edited = call(&mut a, "text_edit", json!({"doc":user,"page":2,"line":1,"text":"Replaced"}));
    assert!(edited["line"]["text"].is_null(), "{edited}");
    withheld("text_edit", &edited);

    let refusals = [
        ("doc_export_text", json!({"doc":user,"path":"out.txt"})),
        ("doc_export_all_images", json!({"doc":user,"folder":"images"})),
        ("doc_export_office", json!({"doc":user,"path":"out.html"})),
        ("doc_export_office", json!({"doc":user,"path":"out.docx"})),
        ("doc_compare", json!({"doc":plain,"other":user})),
        ("doc_compare", json!({"doc":user,"other":plain})),
        ("doc_compare_report", json!({"doc":plain,"other":user,"path":"report.pdf"})),
        ("doc_compare_mark", json!({"doc":plain,"other":user})),
    ];
    for (tool, args) in &refusals {
        let error = a.call(tool, args).unwrap_err().to_string();
        assert!(error.contains("does not allow copying"), "{tool} {args}: {error}");
        no_leak(tool, &error);
    }
    for file in ["out.txt", "out.html", "out.docx", "report.pdf"] {
        assert!(!root.join(file).exists(), "{file} must not be written");
    }
    assert!(std::fs::read_dir(root.join("images")).into_iter().flatten().next().is_none(), "no image is extracted");
    assert_eq!(call(&mut a, "doc_info", json!({"doc":plain}))["document"]["undo"], Value::Null, "a refused mark adds nothing");

    let found = call(&mut a, "text_find", json!({"doc":owner,"query":"first ROW"}));
    assert_eq!(found["matches"][0]["text"], "First row");
    assert!(found.get("text_withheld").is_none());
    let lines = call(&mut a, "text_lines", json!({"doc":owner,"page":1}));
    assert!(lines["lines"].as_array().unwrap().iter().any(|l| l["text"] == "First row"), "{lines}");
    assert!(call(&mut a, "text_paragraphs", json!({"doc":owner,"page":1}))["paragraphs"].to_string().contains("First row"));
    assert_eq!(call(&mut a, "content_list", json!({"doc":owner}))["items"][0]["text"], "Added secret");
    call(&mut a, "doc_export_text", json!({"doc":owner,"path":"owner.txt"}));
    assert!(std::fs::read_to_string(root.join("owner.txt")).unwrap().contains("First row"));
    call(&mut a, "doc_export_office", json!({"doc":owner,"path":"owner.html"}));
    assert!(std::fs::read_to_string(root.join("owner.html")).unwrap().contains("First row"));
    call(&mut a, "doc_export_all_images", json!({"doc":owner,"folder":"owner-images"}));
    call(&mut a, "doc_compare", json!({"doc":owner,"other":plain}));
    let _ = std::fs::remove_dir_all(root);
}
/// MCP text resources read through text_extract, so they refuse too, without echoing text.
#[cfg(feature = "mcp")]
#[test]
fn mcp_text_resources_refuse_copy_denied_documents_and_owner_lifts() {
    use pdfcraft_automation::mcp::McpServer;
    let (mut a, root, id) = setup("mcp-guard");
    call(&mut a, "doc_protect", json!({"doc":id,"open_password":"Mcp-user-test","permissions_password":"Mcp-owner-test","copy":false}));
    call(&mut a, "doc_save", json!({"doc":id,"path":"mcp.pdf"}));
    let user = call(&mut a, "doc_open", json!({"path":"mcp.pdf","password":"Mcp-user-test"}))["doc"].as_u64().unwrap();
    let owner = call(&mut a, "doc_open", json!({"path":"mcp.pdf","password":"Mcp-owner-test"}))["doc"].as_u64().unwrap();
    let mut server = McpServer::new(a);
    let mut read = |uri: String| {
        let line = json!({"jsonrpc":"2.0","id":1,"method":"resources/read","params":{"uri":uri}}).to_string();
        serde_json::from_str::<Value>(&server.handle_line(&line).expect("a reply")).unwrap()
    };
    for uri in [format!("pdfcraft://doc/{user}/text"), format!("pdfcraft://doc/{user}/page/1/text")] {
        let reply = read(uri.clone());
        assert!(reply["error"]["message"].as_str().is_some_and(|m| m.contains("does not allow copying")), "{uri}: {reply}");
        assert!(!reply.to_string().contains("First") && !reply.to_string().contains("Next"), "{uri}: {reply}");
    }
    let info = read(format!("pdfcraft://doc/{user}/info"));
    assert!(info["result"].is_object() && !info.to_string().contains("First row"), "{info}");
    let text = read(format!("pdfcraft://doc/{owner}/text"));
    assert!(text["result"]["contents"][0]["text"].as_str().unwrap().contains("First row"), "{text}");
    let _ = std::fs::remove_dir_all(root);
}
