//! `page_objects`, `objects_arrange`, `objects_align`, `objects_distribute` and
//! `objects_transform` end to end: synthetic pages through the tool table, checked by rendered
//! pixels, object boxes and text extraction, through undo, save and reopen.

use pdfcraft_automation::{Automation, Content, ToolError};
use pdfcraft_cos::{Dict, Document, Object, Stream};
use serde_json::{Value, json};

fn call(app: &mut Automation, name: &str, args: Value) -> Value {
    match app.call(name, &args).unwrap_or_else(|e| panic!("{name}: {e:?}")).remove(0) {
        Content::Json(v) => v,
        _ => panic!("JSON result"),
    }
}

/// A temp root holding `page.pdf`: one 200×200 page with Helvetica as /F1 whose content is
/// `content`.
fn open(name: &str, content: &str) -> (Automation, u64, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("pdfcraft-arrange-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let blank = pdfcraft_engine::Session::new().create_blank(200.0, 200.0, 1).unwrap();
    let mut cos = Document::open(blank).unwrap();
    let page = pdfcraft_model::pages(&cos)[0].obj;
    let stream = cos.add(Object::Stream(Stream::from_raw(Dict::new(), content.as_bytes().to_vec())));
    let mut font = Dict::new();
    font.set(b"Type".to_vec(), Object::name("Font"));
    font.set(b"Subtype".to_vec(), Object::name("Type1"));
    font.set(b"BaseFont".to_vec(), Object::name("Helvetica"));
    font.set(b"Encoding".to_vec(), Object::name("WinAnsiEncoding"));
    let mut fonts = Dict::new();
    fonts.set(b"F1".to_vec(), Object::Dict(font));
    let mut res = Dict::new();
    res.set(b"Font".to_vec(), Object::Dict(fonts));
    cos.update_dict(page, |d| {
        d.set(b"Contents".to_vec(), Object::Ref(stream));
        d.set(b"Resources".to_vec(), Object::Dict(res));
        d.set(b"MediaBox".to_vec(), Object::Array(vec![Object::Int(0), Object::Int(0), Object::Int(200), Object::Int(200)]));
    })
    .unwrap();
    std::fs::write(dir.join("page.pdf"), pdfcraft_cos::write_full(&cos, &Default::default()).unwrap()).unwrap();
    let mut app = Automation::new().with_root(&dir).unwrap();
    let doc = call(&mut app, "doc_open", json!({"path":"page.pdf"}))["doc"].as_u64().unwrap();
    (app, doc, dir)
}

fn pixels(a: &mut Automation, doc: u64) -> image::RgbaImage {
    let rendered = a.call("page_render", &json!({ "doc": doc, "page": 1, "dpi": 72 })).unwrap();
    let Content::Png { data, .. } = &rendered[0] else { panic!("expected page PNG") };
    image::load_from_memory(data).unwrap().to_rgba8()
}

fn red(p: &image::Rgba<u8>) -> bool {
    p[0] > 200 && p[1] < 80 && p[2] < 80
}

fn blue(p: &image::Rgba<u8>) -> bool {
    p[2] > 200 && p[0] < 80 && p[1] < 80
}

fn rects(a: &mut Automation, doc: u64) -> Vec<Vec<f64>> {
    call(a, "page_objects", json!({ "doc": doc, "page": 1 }))["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["rect"].as_array().map(|r| r.iter().map(|v| v.as_f64().unwrap()).collect()).unwrap_or_default())
        .collect()
}

fn lines(a: &mut Automation, doc: u64) -> Vec<String> {
    call(a, "text_lines", json!({ "doc": doc, "page": 1 }))["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["text"].as_str().unwrap().to_string())
        .collect()
}

/// A red square under a blue one (overlapping at user 50–80), and a line of blue text.
const PAGE: &str = "q 1 0 0 rg 20 20 60 60 re f Q 0 0 1 rg 50 50 60 60 re f BT /F1 12 Tf 20 150 Td (Hello) Tj ET";

#[test]
fn arranging_objects_changes_what_is_on_top() {
    let (mut a, doc, dir) = open("order", PAGE);
    let list = call(&mut a, "page_objects", json!({ "doc": doc, "page": 1 }));
    assert_eq!(list["count"], 3);
    assert_eq!(list["objects"][0]["kind"], "path");
    assert_eq!(list["objects"][0]["rect"], json!([20.0, 120.0, 80.0, 180.0]), "top-left origin");
    assert_eq!(list["objects"][2]["kind"], "text");
    assert_eq!(list["objects"][2]["text"], "Hello");
    // The overlap, at user (65, 65): view (65, 135).
    let at = |img: &image::RgbaImage| *img.get_pixel(65, 135);
    assert!(blue(&at(&pixels(&mut a, doc))));

    let r = call(&mut a, "objects_arrange", json!({ "doc": doc, "page": 1, "objects": [1], "order": "front" }));
    assert_eq!(r["undo"], "Bring to front");
    assert!(red(&at(&pixels(&mut a, doc))), "the red square is on top now");
    // The text kept its inherited blue and is still found as text.
    assert_eq!(lines(&mut a, doc), ["Hello"]);
    let text_pixels = pixels(&mut a, doc);
    let found_blue_text = (20..60).any(|x| (40..55).any(|y| blue(text_pixels.get_pixel(x, y))));
    assert!(found_blue_text, "the text still draws in blue");
    // Send Backward puts it back under the blue square it overlaps.
    let r = call(&mut a, "objects_arrange", json!({ "doc": doc, "page": 1, "objects": [3], "order": "backward" }));
    assert_eq!(r["undo"], "Send backward");
    assert!(blue(&at(&pixels(&mut a, doc))));
    call(&mut a, "edit_undo", json!({ "doc": doc }));
    assert!(red(&at(&pixels(&mut a, doc))), "one undo step");

    // Survives save and reopen.
    call(&mut a, "doc_save", json!({ "doc": doc, "path": "out.pdf" }));
    let re = call(&mut a, "doc_open", json!({ "path": "out.pdf" }))["doc"].as_u64().unwrap();
    assert!(red(&at(&pixels(&mut a, re))));
    assert_eq!(lines(&mut a, re), ["Hello"]);
    assert_eq!(call(&mut a, "page_objects", json!({ "doc": re, "page": 1 }))["objects"][2]["kind"], "path");

    // Bad arguments are refused with a reason.
    for args in [
        json!({ "doc": doc, "page": 1, "objects": [4], "order": "front" }),
        json!({ "doc": doc, "page": 1, "objects": [], "order": "front" }),
        json!({ "doc": doc, "page": 1, "objects": [1], "order": "sideways" }),
        json!({ "doc": doc, "page": 2, "objects": [1], "order": "front" }),
    ] {
        assert!(matches!(a.call("objects_arrange", &args), Err(ToolError::InvalidArgs(_))), "{args}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn aligning_distributing_and_turning_objects_moves_them_as_displayed() {
    let content = "0 g 10 10 20 20 re f 0 g 90 40 30 30 re f BT /F1 12 Tf 150 100 Td (Hi) Tj ET";
    let (mut a, doc, dir) = open("align", content);
    let r = call(&mut a, "objects_align", json!({ "doc": doc, "page": 1, "objects": [1, 2, 3], "align": "left" }));
    assert_eq!(r["undo"], "Align left");
    let after = rects(&mut a, doc);
    for r in &after {
        assert!((r[0] - 10.0).abs() < 0.01, "{after:?}");
    }
    assert_eq!(lines(&mut a, doc), ["Hi"], "aligned text is still text");
    call(&mut a, "edit_undo", json!({ "doc": doc }));
    assert!((rects(&mut a, doc)[2][0] - 150.0).abs() < 0.01, "one undo step");

    // Distribute the two squares and the text horizontally: equal gaps.
    call(&mut a, "objects_distribute", json!({ "doc": doc, "page": 1, "objects": [1, 2, 3], "axis": "horizontal" }));
    let r = rects(&mut a, doc);
    let (gap1, gap2) = (r[1][0] - r[0][2], r[2][0] - r[1][2]);
    assert!((gap1 - gap2).abs() < 0.01, "{r:?}");
    assert!(a.call("objects_distribute", &json!({ "doc": doc, "page": 1, "objects": [1, 2], "axis": "vertical" })).is_err());

    // Move the first square 30 right and 40 up (view y runs down), then turn the text a quarter.
    let r = call(&mut a, "objects_transform", json!({ "doc": doc, "page": 1, "objects": [1], "action": "move", "dx": 30, "dy": -40 }));
    assert_eq!(r["undo"], "Move objects");
    assert_eq!(rects(&mut a, doc)[0], vec![40.0, 130.0, 60.0, 150.0]);
    let img = pixels(&mut a, doc);
    let dark = |p: &image::Rgba<u8>| p[0] < 60 && p[1] < 60 && p[2] < 60;
    assert!(dark(img.get_pixel(50, 140)) && !dark(img.get_pixel(20, 180)), "the square is drawn where it moved to");
    let b = rects(&mut a, doc)[2].clone();
    call(&mut a, "objects_transform", json!({ "doc": doc, "page": 1, "objects": [3], "action": "rotate" }));
    let t = rects(&mut a, doc)[2].clone();
    assert!(((t[2] - t[0]) - (b[3] - b[1])).abs() < 0.05 && ((t[3] - t[1]) - (b[2] - b[0])).abs() < 0.05, "turned a quarter: {b:?} → {t:?}");
    assert_eq!(lines(&mut a, doc), ["Hi"]);
    call(&mut a, "objects_transform", json!({ "doc": doc, "page": 1, "objects": [2, 3], "action": "flip_horizontal" }));
    assert_eq!(lines(&mut a, doc), ["Hi"]);
    assert!(matches!(
        a.call("objects_transform", &json!({ "doc": doc, "page": 1, "objects": [1], "action": "spin" })),
        Err(ToolError::InvalidArgs(_))
    ));
    let _ = std::fs::remove_dir_all(dir);
}

/// A tagged page: an H1 (MCID 0) over an untagged box.
fn tagged_pdf() -> Vec<u8> {
    let c = "0 0 1 rg 10 10 180 60 re f /H1 << /MCID 0 >> BDC BT /F1 12 Tf 20 30 Td (Heading) Tj ET EMC";
    let objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 6 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 200] /Resources << /Font << /F1 5 0 R >> >> >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R /StructParents 0 >>".to_vec(),
        format!("<< /Length {} >>\nstream\n{c}\nendstream", c.len()).into_bytes(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        b"<< /Type /StructTreeRoot /K 7 0 R /ParentTree 8 0 R /ParentTreeNextKey 1 >>".to_vec(),
        b"<< /Type /StructElem /S /H1 /P 6 0 R /Pg 3 0 R /K 0 >>".to_vec(),
        b"<< /Nums [0 [7 0 R]] >>".to_vec(),
    ];
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

#[test]
fn tagged_objects_keep_their_structure_when_arranged_and_moved() {
    let dir = std::env::temp_dir().join(format!("pdfcraft-arrange-tagged-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("tagged.pdf"), tagged_pdf()).unwrap();
    let mut a = Automation::new().with_root(&dir).unwrap();
    let doc = call(&mut a, "doc_open", json!({ "path": "tagged.pdf" }))["doc"].as_u64().unwrap();
    let list = call(&mut a, "page_objects", json!({ "doc": doc, "page": 1 }));
    assert_eq!(list["objects"][1]["mcid"], 0);
    for (tool, args) in [
        ("objects_arrange", json!({ "doc": doc, "page": 1, "objects": [2], "order": "back" })),
        ("objects_transform", json!({ "doc": doc, "page": 1, "objects": [1], "action": "move", "dx": 0, "dy": -100 })),
    ] {
        let r = call(&mut a, tool, args);
        if let Some(t) = r.get("tags") {
            assert_eq!((t["removed"].as_u64(), t["pruned"].as_u64()), (Some(0), Some(0)), "{t}");
            assert!(t["added"].as_array().unwrap().is_empty() && t["warnings"].as_array().unwrap().is_empty(), "{t}");
        }
    }
    let list = call(&mut a, "page_objects", json!({ "doc": doc, "page": 1 }));
    assert_eq!((list["objects"][0]["mcid"].clone(), list["objects"][0]["kind"].clone()), (json!(0), json!("text")));
    // Blue box drawn over the heading now: the heading still extracts, still tagged.
    assert_eq!(lines(&mut a, doc), ["Heading"]);
    let _ = std::fs::remove_dir_all(dir);
}
