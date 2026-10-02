//! End-to-end tests of the automation tools and the MCP server, on synthetic PDFs.

use std::path::{Path, PathBuf};

#[cfg(feature = "mcp")]
use printcraft_automation::mcp::McpServer;
use printcraft_automation::{Automation, Content, ToolError, tools};
use serde_json::{Value, json};

/// A PDF with `n` 200×300 pt pages reading "Page 1", "Page 2", …
fn fixture(n: usize) -> Vec<u8> {
    let mut objs: Vec<Vec<u8>> = vec![b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()];
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
    objs.push(format!("<< /Type /Pages /Kids [{}] /Count {n} /MediaBox [0 0 200 300] >>", kids.join(" ")).into_bytes());
    objs.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec());
    for i in 0..n {
        objs.push(format!("<< /Type /Page /Parent 2 0 R /Contents {} 0 R /Resources << /Font << /F1 3 0 R >> >> >>", 5 + 2 * i).into_bytes());
        let body = format!("BT /F1 24 Tf 20 150 Td (Page {}) Tj ET", i + 1);
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

/// A fresh directory with `a.pdf` (3 pages) and `b.pdf` (2 pages).
fn workdir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("printcraft-automation-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.pdf"), fixture(3)).unwrap();
    std::fs::write(dir.join("b.pdf"), fixture(2)).unwrap();
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

fn page_text(a: &mut Automation, doc: u64) -> Vec<String> {
    ok(a, "text_extract", json!({ "doc": doc }))["pages"].as_array().unwrap().iter().map(|p| p["text"].as_str().unwrap().trim().to_string()).collect()
}

#[test]
fn tool_table_is_well_formed() {
    let all = tools();
    let mut names = std::collections::HashSet::new();
    for t in &all {
        assert!(names.insert(t.name), "duplicate tool {}", t.name);
        assert!(t.name.len() <= 64 && t.name.chars().all(|c| c.is_ascii_lowercase() || c == '_'), "bad MCP tool name {}", t.name);
        assert_eq!(t.input_schema["type"], "object", "{}", t.name);
        let props = t.input_schema["properties"].as_object().unwrap();
        for r in t.input_schema["required"].as_array().unwrap() {
            assert!(props.contains_key(r.as_str().unwrap()), "{}: required {r} is not a property", t.name);
        }
        assert!(!(t.read_only && t.destructive), "{} is both read-only and destructive", t.name);
        if let Some(c) = t.command {
            assert!(printcraft_engine::commands::command(c).is_some(), "{} names unregistered command {c}", t.name);
        }
    }
}

#[test]
fn open_inspect_render_and_find() {
    let dir = workdir("inspect");
    let mut a = auto(&dir);
    let opened = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }));
    assert_eq!(opened["pages"], 3);
    assert_eq!(opened["editable"], true);
    let doc = opened["doc"].as_u64().unwrap();

    let info = ok(&mut a, "doc_info", json!({ "doc": doc }));
    assert_eq!(info["pages"].as_array().unwrap().len(), 3);
    assert_eq!(info["pages"][0]["width"], 200.0);
    assert_eq!(info["pages"][1]["label"], "2");

    assert_eq!(page_text(&mut a, doc), ["Page 1", "Page 2", "Page 3"]);

    let found = ok(&mut a, "text_find", json!({ "doc": doc, "query": "page  2" }));
    assert_eq!(found["count"], 1);
    assert_eq!(found["matches"][0]["page"], 2);
    let rect = found["matches"][0]["rects"][0].as_array().unwrap();
    assert!(rect[1].as_f64().unwrap() > 100.0 && rect[3].as_f64().unwrap() < 160.0, "rect is top-left based: {rect:?}");

    let png = a.call("page_render", &json!({ "doc": doc, "page": 1, "dpi": 72 })).unwrap();
    let Content::Png { data, width, height } = &png[0] else { panic!("expected an image") };
    assert_eq!((*width, *height), (200, 300));
    assert_eq!(&data[..8], b"\x89PNG\r\n\x1a\n");
    let decoder = png::Decoder::new(std::io::Cursor::new(data.as_slice()));
    let info = decoder.read_info().unwrap().info().clone();
    assert_eq!((info.width, info.height), (200, 300));
}

#[test]
fn edit_undo_redo_and_save_round_trip() {
    let dir = workdir("edit");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();

    let s = ok(&mut a, "page_delete", json!({ "doc": doc, "pages": [2] }));
    assert_eq!((s["pages"].as_u64(), s["dirty"].as_bool()), (Some(2), Some(true)));
    assert_eq!(page_text(&mut a, doc), ["Page 1", "Page 3"]);

    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    assert_eq!(page_text(&mut a, doc), ["Page 1", "Page 2", "Page 3"]);
    ok(&mut a, "edit_redo", json!({ "doc": doc }));
    assert_eq!(page_text(&mut a, doc), ["Page 1", "Page 3"]);

    ok(&mut a, "page_insert_file", json!({ "doc": doc, "path": "b.pdf", "pages": [2], "at": 1 }));
    ok(&mut a, "page_insert_blank", json!({ "doc": doc, "at": 4 }));
    ok(&mut a, "page_move", json!({ "doc": doc, "pages": [3], "to": 1 }));
    ok(&mut a, "page_rotate", json!({ "doc": doc, "pages": [1], "degrees": 90 }));
    ok(&mut a, "doc_set_info", json!({ "doc": doc, "key": "Title", "value": "Automated" }));
    assert_eq!(page_text(&mut a, doc), ["Page 3", "Page 2", "Page 1", ""]);

    let saved = ok(&mut a, "doc_save", json!({ "doc": doc, "path": "out/edited.pdf" }));
    assert_eq!(saved["incremental"], false);
    assert_eq!(saved["document"]["dirty"], false);

    let mut b = auto(&dir);
    let re = ok(&mut b, "doc_open", json!({ "path": "out/edited.pdf" }))["doc"].as_u64().unwrap();
    let info = ok(&mut b, "doc_info", json!({ "doc": re }));
    assert_eq!(info["title"], "Automated");
    assert_eq!(info["pages"][0]["rotation"], 90);
    assert_eq!(page_text(&mut b, re), ["Page 3", "Page 2", "Page 1", ""]);

    // Saving in place appends an incremental update.
    ok(&mut b, "doc_set_info", json!({ "doc": re, "key": "Author", "value": "Agent" }));
    let before = std::fs::metadata(dir.join("out/edited.pdf")).unwrap().len();
    let again = ok(&mut b, "doc_save", json!({ "doc": re }));
    assert_eq!(again["incremental"], true);
    let after = std::fs::read(dir.join("out/edited.pdf")).unwrap();
    assert!(after.len() as u64 > before);
    assert_eq!(after.windows(5).filter(|w| w == b"%%EOF").count(), 2);
}

#[test]
fn combine_extract_and_split() {
    let dir = workdir("organize");
    let mut a = auto(&dir);
    let combined = ok(&mut a, "doc_combine", json!({ "paths": ["a.pdf", "b.pdf"], "out": "ab.pdf", "open": true }));
    let doc = combined["document"]["doc"].as_u64().unwrap();
    assert_eq!(combined["document"]["pages"], 5);
    assert_eq!(page_text(&mut a, doc), ["Page 1", "Page 2", "Page 3", "Page 1", "Page 2"]);

    let ex = ok(&mut a, "page_extract", json!({ "doc": doc, "pages": [2, 4] }));
    let ex_doc = ex["document"]["doc"].as_u64().unwrap();
    assert_eq!(page_text(&mut a, ex_doc), ["Page 2", "Page 1"]);
    assert!(ex.get("path").is_none());

    let split = ok(&mut a, "doc_split", json!({ "doc": doc, "every": 2, "out_dir": "parts" }));
    let files = split["files"].as_array().unwrap();
    assert_eq!(files.len(), 3);
    assert_eq!((files[2]["first_page"].as_u64(), files[2]["last_page"].as_u64()), (Some(5), Some(5)));
    for f in files {
        assert!(Path::new(f["path"].as_str().unwrap()).starts_with(dir.canonicalize().unwrap()));
    }
}

#[test]
fn errors_are_specific_and_safe() {
    let dir = workdir("errors");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();

    let err = |a: &mut Automation, tool: &str, args: Value| a.call(tool, &args).unwrap_err();
    assert!(matches!(err(&mut a, "nope", json!({})), ToolError::UnknownTool(_)));
    assert!(matches!(err(&mut a, "page_delete", json!({ "doc": doc, "pages": [9] })), ToolError::InvalidArgs(m) if m.contains("3 pages")));
    assert!(matches!(err(&mut a, "page_delete", json!({ "doc": doc, "pages": [0] })), ToolError::InvalidArgs(_)));
    assert!(matches!(err(&mut a, "page_delete", json!({ "doc": doc, "page": [1] })), ToolError::InvalidArgs(m) if m.contains("unknown argument")));
    assert!(matches!(err(&mut a, "page_rotate", json!({ "doc": doc, "pages": [1], "degrees": 45 })), ToolError::InvalidArgs(_)));
    assert!(matches!(err(&mut a, "doc_info", json!({ "doc": 99 })), ToolError::Failed(_)));
    assert!(matches!(err(&mut a, "edit_undo", json!({ "doc": doc })), ToolError::Failed(_)));

    // Unsaved changes are never dropped silently.
    ok(&mut a, "page_delete", json!({ "doc": doc, "pages": [1] }));
    assert!(matches!(err(&mut a, "doc_close", json!({ "doc": doc })), ToolError::Failed(m) if m.contains("unsaved")));
    ok(&mut a, "doc_close", json!({ "doc": doc, "discard_changes": true }));
    assert_eq!(ok(&mut a, "doc_list", json!({}))["documents"], json!([]));

    // The root confines reads and writes.
    let outside = std::env::temp_dir().join("printcraft-automation-outside.pdf");
    std::fs::write(&outside, fixture(1)).unwrap();
    assert!(matches!(err(&mut a, "doc_open", json!({ "path": outside.to_str().unwrap() })), ToolError::Failed(m) if m.contains("outside")));
    assert!(
        matches!(err(&mut a, "doc_open", json!({ "path": "../printcraft-automation-outside.pdf" })), ToolError::Failed(m) if m.contains("outside"))
    );
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    assert!(a.call("doc_save", &json!({ "doc": doc, "path": "new/../../escape.pdf" })).is_err());
    assert!(a.call("doc_save", &json!({ "doc": doc, "path": outside.to_str().unwrap() })).is_err());
    let _ = std::fs::remove_file(outside);
}

#[test]
fn command_list_reports_enablement_and_tools() {
    let dir = workdir("commands");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    let list = ok(&mut a, "command_list", json!({ "doc": doc }));
    let find = |id: &str| list["commands"].as_array().unwrap().iter().find(|c| c["id"] == id).unwrap().clone();
    assert_eq!(find("page.rotate")["tool"], "page_rotate");
    assert_eq!(find("page.rotate")["enabled"], true);
    assert_eq!(find("edit.undo")["enabled"], false);
    ok(&mut a, "page_rotate", json!({ "doc": doc, "pages": [1], "degrees": 90 }));
    let list = ok(&mut a, "command_list", json!({ "doc": doc }));
    let undo = list["commands"].as_array().unwrap().iter().find(|c| c["id"] == "edit.undo").unwrap().clone();
    assert_eq!(undo["enabled"], true);
    assert_eq!(undo["label"], "Undo Rotate page");
}

// ---- MCP ---------------------------------------------------------------------------------------

#[cfg(feature = "mcp")]
fn rpc(server: &mut McpServer, id: u64, method: &str, params: Value) -> Value {
    let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string();
    serde_json::from_str(&server.handle_line(&line).expect("a reply")).unwrap()
}

#[cfg(feature = "mcp")]
#[test]
fn mcp_session_over_stdio() {
    let dir = workdir("mcp");
    let path = dir.join("a.pdf");
    let input = [
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": { "name": "test", "version": "0" } } }),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "doc_open", "arguments": { "path": path } } }),
        json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": { "name": "page_render", "arguments": { "doc": 1, "page": 2, "dpi": 36 } } }),
    ]
    .iter()
    .map(Value::to_string)
    .collect::<Vec<_>>()
    .join("\n");
    let mut out = Vec::new();
    McpServer::new(Automation::new()).serve(input.as_bytes(), &mut out).unwrap();
    let replies: Vec<Value> = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(replies.len(), 4, "the notification gets no reply");

    assert_eq!(replies[0]["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(replies[0]["result"]["serverInfo"]["name"], "printcraft");
    assert_eq!(replies[1]["result"]["tools"].as_array().unwrap().len(), tools().len());
    assert_eq!(replies[2]["result"]["structuredContent"]["pages"], 3);
    assert_eq!(replies[2]["result"]["isError"], false);
    let img = &replies[3]["result"]["content"][0];
    assert_eq!((img["type"].as_str(), img["mimeType"].as_str()), (Some("image"), Some("image/png")));
    use base64::Engine as _;
    let png = base64::engine::general_purpose::STANDARD.decode(img["data"].as_str().unwrap()).unwrap();
    assert_eq!(&png[1..4], b"PNG");
}

#[cfg(feature = "mcp")]
#[test]
fn mcp_errors() {
    let mut s = McpServer::new(Automation::new());
    assert_eq!(rpc(&mut s, 1, "initialize", json!({ "protocolVersion": "1999-01-01" }))["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(rpc(&mut s, 2, "ping", json!({}))["result"], json!({}));
    assert_eq!(rpc(&mut s, 3, "resources/list", json!({}))["error"]["code"], -32601);
    assert_eq!(rpc(&mut s, 4, "tools/call", json!({ "name": "nope" }))["error"]["code"], -32602);
    let failed = rpc(&mut s, 5, "tools/call", json!({ "name": "doc_open", "arguments": { "path": "/definitely/not/here.pdf" } }));
    assert_eq!(failed["result"]["isError"], true);
    assert!(failed["result"]["content"][0]["text"].as_str().unwrap().contains("here.pdf"));
    let bad: Value = serde_json::from_str(&s.handle_line("{not json").unwrap()).unwrap();
    assert_eq!(bad["error"]["code"], -32700);
}

#[test]
fn parallel_text_extraction_keeps_page_order_and_follows_edits() {
    let dir = workdir("parallel");
    std::fs::write(dir.join("long.pdf"), fixture(40)).unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "long.pdf" }))["doc"].as_u64().unwrap();
    let expected: Vec<String> = (1..=40).map(|i| format!("Page {i}")).collect();
    assert_eq!(page_text(&mut a, doc), expected);
    assert_eq!(ok(&mut a, "text_find", json!({ "doc": doc, "query": "page 3" }))["count"], 11); // 3, 30–39
    ok(&mut a, "page_delete", json!({ "doc": doc, "pages": [3] }));
    assert_eq!(ok(&mut a, "text_find", json!({ "doc": doc, "query": "page 3" }))["count"], 10);
    assert_eq!(page_text(&mut a, doc)[2], "Page 4");
}

#[test]
fn bookmarks_through_tools() {
    let dir = workdir("bookmarks");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    assert_eq!(ok(&mut a, "bookmark_list", json!({ "doc": doc }))["bookmarks"], json!([]));
    ok(&mut a, "bookmark_add", json!({ "doc": doc, "title": "Intro", "page": 1 }));
    ok(&mut a, "bookmark_add", json!({ "doc": doc, "title": "Body", "page": 2 }));
    ok(&mut a, "bookmark_add", json!({ "doc": doc, "title": "Detail", "page": 3, "parent": [2] }));
    ok(&mut a, "bookmark_move", json!({ "doc": doc, "path": [1], "parent": [2], "position": 1 })); // Intro under Body
    ok(&mut a, "bookmark_rename", json!({ "doc": doc, "path": [1, 2], "title": "Details" }));
    ok(&mut a, "bookmark_set_page", json!({ "doc": doc, "path": [1, 1], "page": 3 }));
    let list = ok(&mut a, "bookmark_list", json!({ "doc": doc }))["bookmarks"].clone();
    assert_eq!(list[0]["title"], "Body");
    assert_eq!(list[0]["children"][0]["title"], "Intro");
    assert_eq!(list[0]["children"][0]["page"], 3);
    assert_eq!(list[0]["children"][0]["path"], json!([1, 1]));
    assert_eq!(list[0]["children"][1]["title"], "Details");
    assert!(matches!(a.call("bookmark_delete", &json!({ "doc": doc, "path": [9] })), Err(ToolError::Failed(_))));
    assert!(matches!(a.call("bookmark_delete", &json!({ "doc": doc, "path": [0] })), Err(ToolError::InvalidArgs(_))));
    ok(&mut a, "bookmark_delete", json!({ "doc": doc, "path": [1, 1] }));
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "marked.pdf" }));
    let mut b = auto(&dir);
    let re = ok(&mut b, "doc_open", json!({ "path": "marked.pdf" }))["doc"].as_u64().unwrap();
    let list = ok(&mut b, "bookmark_list", json!({ "doc": re }))["bookmarks"].clone();
    assert_eq!((list[0]["title"].as_str(), list[0]["children"][0]["title"].as_str()), (Some("Body"), Some("Details")));
}

#[test]
fn numbering_pages_through_tools() {
    let dir = workdir("labels");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    let r = ok(&mut a, "page_number", json!({ "doc": doc, "from": 1, "to": 1, "style": "upper-roman" }));
    assert_eq!(r["labels"], json!(["I", "2", "3"]));
    let r = ok(&mut a, "page_number", json!({ "doc": doc, "from": 2, "to": 3, "prefix": "B-", "start": 5 }));
    assert_eq!(r["labels"], json!(["I", "B-5", "B-6"]));
    assert!(matches!(a.call("page_number", &json!({ "doc": doc, "from": 2, "to": 4 })), Err(ToolError::InvalidArgs(_))));
    assert!(matches!(a.call("page_number", &json!({ "doc": doc, "from": 1, "to": 1, "style": "klingon" })), Err(ToolError::InvalidArgs(_))));
}

#[test]
fn comments_through_tools() {
    let dir = workdir("comments");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    // "Page 2" sits at y = 150 (user space) = 150 from the top of the 300 pt page.
    let hl = ok(&mut a, "comment_add", json!({ "doc": doc, "page": 2, "type": "highlight", "find": "page 2", "contents": "check", "author": "Ada" }));
    assert_eq!(hl["comment"]["lines"], 1);
    let id = hl["comment"]["id"].as_str().unwrap().to_string();
    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 2, "type": "note", "at": [150, 20], "contents": "Sticky" }));
    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 1, "type": "rectangle", "rect": [10, 10, 60, 40], "color": "blue", "width": 3 }));
    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 1, "type": "textbox", "rect": [10, 200, 190, 240], "contents": "Hello", "font_size": 10 }));
    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 1, "type": "ink", "strokes": [[[10, 280], [50, 260], [90, 285]]] }));
    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 3, "type": "arrow", "from": [10, 10], "to": [100, 100] }));
    ok(&mut a, "comment_reply", json!({ "doc": doc, "id": id, "text": "Looks right", "author": "Bob" }));
    ok(&mut a, "comment_set_status", json!({ "doc": doc, "id": id, "status": "accepted", "author": "Bob" }));

    let list = ok(&mut a, "comment_list", json!({ "doc": doc }));
    assert_eq!(list["count"], 6, "{list}");
    let c = list["comments"].as_array().unwrap().iter().find(|c| c["id"] == id.as_str()).unwrap().clone();
    assert_eq!((c["type"].as_str(), c["author"].as_str(), c["contents"].as_str()), (Some("Highlight"), Some("Ada"), Some("check")));
    assert_eq!(c["status"], "Accepted");
    assert_eq!(c["replies"].as_array().unwrap().len(), 1);
    assert_eq!(c["replies"][0]["contents"], "Looks right");
    // The highlight covers the found text, reported in top-left-origin points.
    let r: Vec<f64> = c["rect"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    assert!(r[0] >= 15.0 && r[0] <= 25.0 && r[1] > 120.0 && r[3] < 160.0, "{r:?}");
    let rect = list["comments"].as_array().unwrap().iter().find(|c| c["type"] == "Square").unwrap().clone();
    assert_eq!(rect["color"], "#0078D6");
    assert_eq!(rect["rect"], json!([10.0, 10.0, 60.0, 40.0]));
    let note = list["comments"].as_array().unwrap().iter().find(|c| c["type"] == "Text").unwrap().clone();
    assert_eq!(note["rect"], json!([150.0, 20.0, 170.0, 40.0]), "note icon hangs from its top-left point");

    // Edit by page + index; several changes are one undo step.
    let (page, index) = (rect["page"].as_u64().unwrap(), rect["index"].as_u64().unwrap());
    ok(&mut a, "comment_edit", json!({ "doc": doc, "page": page, "index": index, "color": "#FF0000", "move": [5, 5], "contents": "moved" }));
    let list = ok(&mut a, "comment_list", json!({ "doc": doc, "page": 1 }));
    let rect = list["comments"].as_array().unwrap().iter().find(|c| c["type"] == "Square").unwrap().clone();
    assert_eq!((rect["color"].as_str(), rect["contents"].as_str()), (Some("#FF0000"), Some("moved")));
    assert_eq!(rect["rect"], json!([15.0, 15.0, 65.0, 45.0]));
    let undo = ok(&mut a, "edit_undo", json!({ "doc": doc }));
    assert_eq!(undo["undone"], "Edit comment");
    ok(&mut a, "edit_redo", json!({ "doc": doc }));

    // The rendered page shows the rectangle's border.
    let png = a.call("page_render", &json!({ "doc": doc, "page": 1, "dpi": 72 })).unwrap();
    assert!(matches!(png[0], Content::Png { .. }));

    ok(&mut a, "comment_delete", json!({ "doc": doc, "id": id }));
    assert_eq!(ok(&mut a, "comment_list", json!({ "doc": doc }))["count"], 5);
    assert!(matches!(a.call("comment_delete", &json!({ "doc": doc, "id": id })), Err(ToolError::Failed(_))));
    assert!(matches!(a.call("comment_add", &json!({ "doc": doc, "page": 1, "type": "highlight", "find": "nowhere" })), Err(ToolError::Failed(_))));
    assert!(matches!(a.call("comment_add", &json!({ "doc": doc, "page": 1, "type": "rectangle" })), Err(ToolError::InvalidArgs(_))));
    assert!(matches!(
        a.call("comment_add", &json!({ "doc": doc, "page": 1, "type": "note", "at": [1, 1], "color": "mauve" })),
        Err(ToolError::InvalidArgs(_))
    ));
    assert!(matches!(a.call("comment_edit", &json!({ "doc": doc, "page": 1, "index": 1 })), Err(ToolError::InvalidArgs(_))));

    // Comments survive a save and reopen.
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "commented.pdf" }));
    let mut b = auto(&dir);
    let re = ok(&mut b, "doc_open", json!({ "path": "commented.pdf" }))["doc"].as_u64().unwrap();
    assert_eq!(ok(&mut b, "comment_list", json!({ "doc": re }))["count"], 5);
}

#[test]
fn protecting_through_tools() {
    let dir = workdir("protect");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    assert_eq!(ok(&mut a, "doc_info", json!({ "doc": doc }))["security"]["protected"], false);
    assert!(matches!(a.call("doc_protect", &json!({ "doc": doc })), Err(ToolError::Failed(_))), "a password is required");
    let r = ok(
        &mut a,
        "doc_protect",
        json!({ "doc": doc, "open_password": "open", "permissions_password": "boss", "printing": "low", "changes": "comment-fill-sign" }),
    );
    assert_eq!(r["security"]["pending"], true);
    assert_eq!(
        (r["security"]["printing"].as_str(), r["security"]["annotate"].as_bool(), r["security"]["copy"].as_bool()),
        (Some("low"), Some(true), Some(false))
    );
    assert!(!r.to_string().contains("boss"), "passwords are never echoed");
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "locked.pdf" }));
    // Another session: the open password is required, and the restrictions hold.
    let mut b = auto(&dir);
    assert!(matches!(b.call("doc_open", &json!({ "path": "locked.pdf" })), Err(ToolError::Failed(_))));
    let re = ok(&mut b, "doc_open", json!({ "path": "locked.pdf", "password": "open" }))["doc"].as_u64().unwrap();
    assert!(matches!(b.call("page_delete", &json!({ "doc": re, "pages": [1] })), Err(ToolError::Failed(_))));
    assert!(matches!(b.call("doc_unprotect", &json!({ "doc": re })), Err(ToolError::Failed(_))));
    ok(&mut b, "comment_add", json!({ "doc": re, "page": 1, "type": "note", "at": [10, 10], "contents": "allowed" }));
    // With the permissions password everything is possible, including removing security.
    let owner = ok(&mut b, "doc_open", json!({ "path": "locked.pdf", "password": "boss" }))["doc"].as_u64().unwrap();
    assert_eq!(ok(&mut b, "doc_unprotect", json!({ "doc": owner }))["security"]["protected"], false);
    ok(&mut b, "doc_save", json!({ "doc": owner, "path": "open.pdf" }));
    let mut c = auto(&dir);
    ok(&mut c, "doc_open", json!({ "path": "open.pdf" }));
}

#[test]
fn forms_through_tools() {
    let dir = workdir("forms");
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../dist/demo/printcraft-showcase.pdf");
    if !src.exists() {
        eprintln!("skipped: run `cargo xtask demo-pdf` for the showcase form");
        return;
    }
    std::fs::copy(&src, dir.join("show.pdf")).unwrap();
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "show.pdf" }))["doc"].as_u64().unwrap();
    let list = ok(&mut a, "form_fields", json!({ "doc": doc }));
    let fields = list["fields"].as_array().unwrap().clone();
    assert!(fields.len() >= 5, "{list}");
    let pick = |t: &str| fields.iter().find(|f| f["type"] == t && f["read_only"] == false).cloned();
    let text = pick("text").expect("a text field");
    let mut values = serde_json::Map::new();
    values.insert(text["name"].as_str().unwrap().into(), json!("Filled by an agent"));
    if let Some(cb) = pick("checkbox") {
        values.insert(cb["name"].as_str().unwrap().into(), json!(true));
    }
    if let Some(combo) = pick("combo") {
        values.insert(combo["name"].as_str().unwrap().into(), combo["options"][1]["label"].clone());
    }
    let r = ok(&mut a, "form_fill", json!({ "doc": doc, "values": values }));
    assert_eq!(r["undo"], "Fill in form");
    let after = ok(&mut a, "form_fields", json!({ "doc": doc }));
    let get = |name: &str| after["fields"].as_array().unwrap().iter().find(|f| f["name"] == name).unwrap()["value"].clone();
    assert_eq!(get(text["name"].as_str().unwrap()), "Filled by an agent");
    // The page shows it.
    let page = text["page"].as_u64().unwrap();
    let found = ok(&mut a, "text_find", json!({ "doc": doc, "query": "Filled by an agent" }));
    assert_eq!(found["count"], 1, "rendered on page {page}");
    assert!(matches!(a.call("form_fill", &json!({ "doc": doc, "values": { "no such field": "x" } })), Err(ToolError::Failed(_))));
    ok(&mut a, "form_reset", json!({ "doc": doc }));
    let reset = ok(&mut a, "form_fields", json!({ "doc": doc }));
    let v = reset["fields"].as_array().unwrap().iter().find(|f| f["name"] == text["name"]).unwrap()["value"].clone();
    assert_ne!(v, "Filled by an agent");
}

#[test]
fn duplicating_and_cropping_through_tools() {
    let dir = workdir("boxes");
    let mut a = auto(&dir);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    let r = ok(&mut a, "page_duplicate", json!({ "doc": doc, "pages": [2] }));
    assert_eq!(r["pages"], 4);
    assert_eq!(page_text(&mut a, doc), ["Page 1", "Page 2", "Page 2", "Page 3"]);
    // Crop page 1 by margins, page 2 to a rect drawn from the top-left.
    let r = ok(&mut a, "page_set_box", json!({ "doc": doc, "pages": [1], "margins": [10, 20, 30, 40] }));
    assert_eq!(r["page_sizes"][0], json!([160.0, 240.0]));
    let r = ok(&mut a, "page_set_box", json!({ "doc": doc, "pages": [2], "rect": [0, 0, 100, 150] }));
    assert_eq!(r["page_sizes"][1], json!([100.0, 150.0]));
    assert_eq!(page_text(&mut a, doc)[1], "Page 2", "the text at y = 150 is still inside");
    // Reset and errors.
    let r = ok(&mut a, "page_set_box", json!({ "doc": doc, "pages": [1] }));
    assert_eq!(r["page_sizes"][0], json!([200.0, 300.0]));
    assert!(matches!(a.call("page_set_box", &json!({ "doc": doc, "margins": [150, 0, 150, 0] })), Err(ToolError::Failed(_))));
    assert!(matches!(a.call("page_set_box", &json!({ "doc": doc, "rect": [0, 0, 10, 10] })), Err(ToolError::InvalidArgs(_))));
}
