//! End-to-end Organize ▸ Insert from clipboard (`page_insert_clipboard`) and Create ▸ Screen
//! capture (`doc_create_screenshot`), including hostile arguments.
use base64::Engine as _;
use pdfcraft_automation::{Automation, Content, ToolError};
use serde_json::{Value, json};
use std::path::PathBuf;

fn ok(a: &mut Automation, name: &str, args: Value) -> Value {
    let mut reply = a.call(name, &args).unwrap_or_else(|e| panic!("{name}: {e}"));
    match reply.remove(0) {
        Content::Json(v) => v,
        other => panic!("expected JSON, got {other:?}"),
    }
}

fn root(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("pdfcraft-clipboard-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().unwrap();
    writer.write_image_data(&[90u8, 140, 200, 255].repeat((w * h) as usize)).unwrap();
    writer.finish().unwrap();
    out
}

/// Page sizes in points, from doc_info.
fn sizes(a: &mut Automation, doc: &Value) -> Vec<(f64, f64)> {
    let info = ok(a, "doc_info", json!({ "doc": doc }));
    info["pages"].as_array().unwrap().iter().map(|p| (p["width"].as_f64().unwrap(), p["height"].as_f64().unwrap())).collect()
}

#[test]
fn clipboard_contents_insert_before_or_after_a_page_in_one_undo_step() {
    let dir = root("insert");
    std::fs::write(dir.join("shot.png"), png(30, 20)).unwrap();
    let mut a = Automation::new().with_root(&dir).unwrap();
    let doc = ok(&mut a, "doc_create", json!({ "from": "blank", "pages": 3 }))["doc"].clone();

    // Raw pixels, before page 2: one page at the image's size.
    let rgba = base64::engine::general_purpose::STANDARD.encode([10u8, 20, 30, 255].repeat(40 * 10));
    let r =
        ok(&mut a, "page_insert_clipboard", json!({ "doc": doc, "page": 2, "position": "before", "rgba_base64": rgba, "width": 40, "height": 10 }));
    assert_eq!((r["pages"].as_u64(), r["inserted"]["first"].as_u64(), r["inserted"]["count"].as_u64()), (Some(4), Some(2), Some(1)));
    assert_eq!(sizes(&mut a, &doc)[1], (40.0, 10.0));

    // Text after the last page (the default) flows onto as many pages as it needs.
    let r = ok(&mut a, "page_insert_clipboard", json!({ "doc": doc, "text": "pasted line\n".repeat(120) }));
    let count = r["inserted"]["count"].as_u64().unwrap();
    assert!(count >= 2, "{r}");
    assert_eq!(r["inserted"]["first"].as_u64(), Some(5));
    assert_eq!(r["pages"].as_u64(), Some(4 + count));
    // One undo step removes them all.
    let r = ok(&mut a, "edit_undo", json!({ "doc": doc }));
    assert_eq!(r["document"]["pages"].as_u64(), Some(4), "{r}");

    // An image file, after page 1.
    let r = ok(&mut a, "page_insert_clipboard", json!({ "doc": doc, "page": 1, "image_path": "shot.png" }));
    assert_eq!(r["inserted"]["first"].as_u64(), Some(2));
    assert_eq!(sizes(&mut a, &doc)[1], (30.0, 20.0));
}

#[test]
fn hostile_clipboard_arguments_are_refused_and_change_nothing() {
    let dir = root("hostile");
    std::fs::write(dir.join("notes.txt"), "not an image").unwrap();
    let mut a = Automation::new().with_root(&dir).unwrap();
    let doc = ok(&mut a, "doc_create", json!({ "from": "blank", "pages": 2 }))["doc"].clone();
    let b64 = |n: usize| base64::engine::general_purpose::STANDARD.encode(vec![0u8; n]);
    for args in [
        json!({ "doc": doc }),
        json!({ "doc": doc, "text": "a", "image_path": "notes.txt" }),
        json!({ "doc": doc, "text": "   " }),
        json!({ "doc": doc, "text": "a", "page": 0 }),
        json!({ "doc": doc, "text": "a", "page": 3 }),
        json!({ "doc": doc, "text": "a", "page": -1 }),
        json!({ "doc": doc, "text": "a", "position": "inside" }),
        json!({ "doc": doc, "image_path": "notes.txt" }),
        json!({ "doc": doc, "image_path": "../outside.png" }),
        json!({ "doc": doc, "rgba_base64": b64(16) }),
        json!({ "doc": doc, "rgba_base64": b64(15), "width": 2, "height": 2 }),
        json!({ "doc": doc, "rgba_base64": b64(16), "width": 0, "height": 4 }),
        json!({ "doc": doc, "rgba_base64": b64(16), "width": -2, "height": -2 }),
        json!({ "doc": doc, "rgba_base64": b64(16), "width": i64::MAX, "height": i64::MAX }),
        json!({ "doc": doc, "rgba_base64": b64(4096), "width": 2, "height": 2 }),
        json!({ "doc": doc, "rgba_base64": "!!not base64!!", "width": 2, "height": 2 }),
    ] {
        let e = a.call("page_insert_clipboard", &args).err();
        assert!(matches!(e, Some(ToolError::InvalidArgs(_) | ToolError::Failed(_))), "{args}: {e:?}");
    }
    assert_eq!(ok(&mut a, "doc_info", json!({ "doc": doc }))["pages"].as_array().map(Vec::len), Some(2));
}

#[test]
fn screen_captures_become_new_documents_whole_or_cut_to_a_region() {
    let dir = root("capture");
    std::fs::write(dir.join("screen.png"), png(64, 48)).unwrap();
    std::fs::write(dir.join("broken.png"), &png(64, 48)[..40]).unwrap();
    let mut a = Automation::new().with_root(&dir).unwrap();
    let whole = ok(&mut a, "doc_create_screenshot", json!({ "path": "screen.png" }));
    assert_eq!((whole["name"].as_str(), whole["pages"].as_u64(), whole["dirty"].as_bool()), (Some("Screen capture.pdf"), Some(1), Some(true)));
    assert_eq!(sizes(&mut a, &whole["doc"]), vec![(64.0, 48.0)]);
    let part = ok(
        &mut a,
        "doc_create_screenshot",
        json!({ "path": "screen.png", "region": { "x": 4, "y": 8, "width": 20, "height": 30 }, "name": "Part.pdf" }),
    );
    assert_eq!(part["name"].as_str(), Some("Part.pdf"));
    assert_eq!(sizes(&mut a, &part["doc"]), vec![(20.0, 30.0)]);

    for args in [
        json!({}),
        json!({ "path": "missing.png" }),
        json!({ "path": "broken.png" }),
        json!({ "path": "../screen.png" }),
        json!({ "path": "screen.png", "region": { "x": 60, "y": 0, "width": 10, "height": 10 } }),
        json!({ "path": "screen.png", "region": { "x": 0, "y": 0, "width": 0, "height": 10 } }),
        json!({ "path": "screen.png", "region": { "x": -1, "y": 0, "width": 4, "height": 4 } }),
        json!({ "path": "screen.png", "region": { "x": 4294967295u64, "y": 0, "width": 4, "height": 4 } }),
        json!({ "path": "screen.png", "region": { "x": 0, "y": 0 } }),
        json!({ "path": "screen.png", "region": "all" }),
    ] {
        assert!(a.call("doc_create_screenshot", &args).is_err(), "{args}");
    }
}
