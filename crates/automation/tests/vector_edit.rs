//! `objects_style`, `objects_resize` and `objects_delete` end to end: synthetic pages through
//! the tool table, checked by rendered pixels and object boxes, through undo, save and reopen,
//! and on a tagged page.

use pdfcraft_automation::{Automation, Content, ToolError};
use serde_json::{Value, json};

fn call(app: &mut Automation, name: &str, args: Value) -> Value {
    match app.call(name, &args).unwrap_or_else(|e| panic!("{name}: {e:?}")).remove(0) {
        Content::Json(v) => v,
        _ => panic!("JSON result"),
    }
}

/// A PDF of the given objects (object 1 the catalog, 2 the pages, 3 the page, 4 its content).
fn pdf(content: &str, page_extra: &str, catalog_extra: &str, more: &[&str]) -> Vec<u8> {
    let mut objs: Vec<Vec<u8>> = vec![
        format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>").into_bytes(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 200] /Resources << /Font << /F1 5 0 R >> >> >>".to_vec(),
        format!("<< /Type /Page /Parent 2 0 R /Contents 4 0 R {page_extra} >>").into_bytes(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()).into_bytes(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
    ];
    objs.extend(more.iter().map(|m| m.as_bytes().to_vec()));
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

fn open(name: &str, bytes: Vec<u8>) -> (Automation, u64, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("pdfcraft-vector-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("page.pdf"), bytes).unwrap();
    let mut app = Automation::new().with_root(&dir).unwrap();
    let doc = call(&mut app, "doc_open", json!({"path":"page.pdf"}))["doc"].as_u64().unwrap();
    (app, doc, dir)
}

fn pixels(a: &mut Automation, doc: u64) -> image::RgbaImage {
    let rendered = a.call("page_render", &json!({ "doc": doc, "page": 1, "dpi": 72 })).unwrap();
    let Content::Png { data, .. } = &rendered[0] else { panic!("expected page PNG") };
    image::load_from_memory(data).unwrap().to_rgba8()
}

fn rects(a: &mut Automation, doc: u64) -> Vec<Vec<f64>> {
    call(a, "page_objects", json!({ "doc": doc, "page": 1 }))["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["rect"].as_array().map(|r| r.iter().map(|v| v.as_f64().unwrap()).collect()).unwrap_or_default())
        .collect()
}

/// A red square (user 20–80) and a black horizontal line at user y 150 from x 20 to 180, 1 pt.
const PAGE: &str = "1 0 0 rg 20 20 60 60 re f 0 G 1 w 20 150 m 180 150 l S";

#[test]
fn styling_paths_changes_colour_width_dash_and_opacity() {
    let (mut a, doc, dir) = open("style", pdf(PAGE, "", "", &[]));
    // Square centre: view (50, 150); the line at view y 50.
    let img = pixels(&mut a, doc);
    let p = *img.get_pixel(50, 150);
    assert!(p[0] > 200 && p[1] < 60, "red first: {p:?}");

    let r = call(&mut a, "objects_style", json!({ "doc": doc, "page": 1, "objects": [1], "fill": "#00ff00", "stroke": [0, 0, 1], "line_width": 6 }));
    assert_eq!(r["undo"], "Change path appearance");
    let img = pixels(&mut a, doc);
    let p = *img.get_pixel(50, 150);
    assert!(p[1] > 200 && p[0] < 60 && p[2] < 60, "green fill: {p:?}");
    // The stroke straddles the square's edge (view x 20): 3 pt outside it is blue now.
    let p = *img.get_pixel(18, 150);
    assert!(p[2] > 200 && p[0] < 60, "blue stroke: {p:?}");

    // The line: 9 pt wide, CMYK red, half opacity, dashed 20 on / 20 off.
    let before_line = (44..57).filter(|y| img.get_pixel(30, *y)[0] < 100).count();
    assert!(before_line <= 2, "a thin line first: {before_line}");
    call(
        &mut a,
        "objects_style",
        json!({ "doc": doc, "page": 1, "objects": [2], "stroke": { "space": "cmyk", "components": [0, 1, 1, 0] }, "line_width": 9, "dash": [20, 20], "cap": "butt", "opacity": 0.5 }),
    );
    let img = pixels(&mut a, doc);
    let on = *img.get_pixel(30, 50);
    let off = *img.get_pixel(50, 50);
    let thick = (40..61).filter(|y| img.get_pixel(30, *y)[1] < 230).count();
    assert!((7..=11).contains(&thick), "about 9 px wide: {thick}");
    // Half-transparent red over white: pink, not solid red.
    assert!(on[0] > 200 && on[1] > 90 && on[1] < 170, "half-opaque red dash: {on:?}");
    assert!(off[0] > 240 && off[1] > 240 && off[2] > 240, "a gap: {off:?}");

    // One undo step brings back the thin solid black line.
    call(&mut a, "edit_undo", json!({ "doc": doc }));
    let img = pixels(&mut a, doc);
    assert!((48..53).any(|y| img.get_pixel(50, y)[0] < 160), "solid again");
    call(&mut a, "edit_redo", json!({ "doc": doc }));

    // Save and reopen.
    call(&mut a, "doc_save", json!({ "doc": doc, "path": "out.pdf" }));
    let re = call(&mut a, "doc_open", json!({ "path": "out.pdf" }))["doc"].as_u64().unwrap();
    let img = pixels(&mut a, re);
    assert_eq!(*img.get_pixel(30, 50), on);
    assert_eq!(*img.get_pixel(50, 150), *pixels(&mut a, doc).get_pixel(50, 150));

    // Bad arguments are refused with a reason.
    for args in [
        json!({ "doc": doc, "page": 1, "objects": [1] }),
        json!({ "doc": doc, "page": 1, "objects": [1], "fill": "#12345" }),
        json!({ "doc": doc, "page": 1, "objects": [1], "fill": [0.1, 0.2] }),
        json!({ "doc": doc, "page": 1, "objects": [1], "fill": [2, 0, 0] }),
        json!({ "doc": doc, "page": 1, "objects": [1], "fill": { "space": "lab", "components": [1] } }),
        json!({ "doc": doc, "page": 1, "objects": [1], "fill": { "components": [0.5] } }),
        json!({ "doc": doc, "page": 1, "objects": [1], "fill": "none", "stroke": "none" }),
        json!({ "doc": doc, "page": 1, "objects": [1], "opacity": 2 }),
        json!({ "doc": doc, "page": 1, "objects": [1], "line_width": -1 }),
        json!({ "doc": doc, "page": 1, "objects": [1], "dash": [0, 0] }),
        json!({ "doc": doc, "page": 1, "objects": [1], "dash_phase": 3 }),
        json!({ "doc": doc, "page": 1, "objects": [1], "cap": "pointy" }),
        json!({ "doc": doc, "page": 1, "objects": [3], "fill": "#000000" }),
    ] {
        assert!(matches!(a.call("objects_style", &args), Err(ToolError::InvalidArgs(_) | ToolError::Failed(_))), "{args}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn resizing_and_deleting_objects() {
    let (mut a, doc, dir) = open("resize", pdf(PAGE, "", "", &[]));
    assert_eq!(rects(&mut a, doc)[0], vec![20.0, 120.0, 80.0, 180.0]);
    let r = call(&mut a, "objects_resize", json!({ "doc": doc, "page": 1, "objects": [1], "width": 120, "anchor": "top_left" }));
    assert_eq!(r["undo"], "Resize objects");
    assert_eq!(rects(&mut a, doc)[0], vec![20.0, 120.0, 140.0, 240.0], "proportional, top-left fixed (view y down)");
    let img = pixels(&mut a, doc);
    assert!(img.get_pixel(130, 130)[0] > 200 && img.get_pixel(130, 130)[1] < 60, "the square is drawn bigger");
    call(&mut a, "objects_resize", json!({ "doc": doc, "page": 1, "objects": [1], "scale_x": 0.5 }));
    assert_eq!(rects(&mut a, doc)[0], vec![50.0, 120.0, 110.0, 240.0], "half as wide about the centre");
    call(&mut a, "edit_undo", json!({ "doc": doc }));
    assert_eq!(rects(&mut a, doc)[0], vec![20.0, 120.0, 140.0, 240.0], "one undo step");
    for args in [
        json!({ "doc": doc, "page": 1, "objects": [1] }),
        json!({ "doc": doc, "page": 1, "objects": [1], "scale": 2, "width": 4 }),
        json!({ "doc": doc, "page": 1, "objects": [1], "scale": 0 }),
        json!({ "doc": doc, "page": 1, "objects": [1], "scale": 1e9 }),
        json!({ "doc": doc, "page": 1, "objects": [1], "scale": 2, "anchor": "middle" }),
        json!({ "doc": doc, "page": 1, "objects": [9], "scale": 2 }),
    ] {
        assert!(matches!(a.call("objects_resize", &args), Err(ToolError::InvalidArgs(_) | ToolError::Failed(_))), "{args}");
    }

    let r = call(&mut a, "objects_delete", json!({ "doc": doc, "page": 1, "objects": [1] }));
    assert_eq!(r["undo"], "Delete objects");
    let list = call(&mut a, "page_objects", json!({ "doc": doc, "page": 1 }));
    assert_eq!(list["count"], 1);
    let img = pixels(&mut a, doc);
    let p = *img.get_pixel(50, 150);
    assert!(p[0] > 240 && p[1] > 240, "the square is gone: {p:?}");
    assert!((48..53).any(|y| img.get_pixel(100, y)[0] < 160), "the line stays");
    call(&mut a, "doc_save", json!({ "doc": doc, "path": "out.pdf" }));
    let re = call(&mut a, "doc_open", json!({ "path": "out.pdf" }))["doc"].as_u64().unwrap();
    assert_eq!(call(&mut a, "page_objects", json!({ "doc": re, "page": 1 }))["count"], 1);
    call(&mut a, "edit_undo", json!({ "doc": doc }));
    assert_eq!(call(&mut a, "page_objects", json!({ "doc": doc, "page": 1 }))["count"], 2, "one undo step");
    let _ = std::fs::remove_dir_all(dir);
}

/// A tagged page: a Figure path (MCID 0) and a P heading (MCID 1).
fn tagged() -> Vec<u8> {
    let c = "/Figure << /MCID 0 >> BDC 0 0 1 rg 10 10 100 60 re f EMC /P << /MCID 1 >> BDC BT /F1 12 Tf 20 150 Td (Words) Tj ET EMC";
    pdf(
        c,
        "/StructParents 0",
        "/MarkInfo << /Marked true >> /StructTreeRoot 6 0 R",
        &[
            "<< /Type /StructTreeRoot /K [7 0 R 8 0 R] /ParentTree 9 0 R /ParentTreeNextKey 1 >>",
            "<< /Type /StructElem /S /Figure /P 6 0 R /Pg 3 0 R /K 0 /Alt (Box) >>",
            "<< /Type /StructElem /S /P /P 6 0 R /Pg 3 0 R /K 1 >>",
            "<< /Nums [0 [7 0 R 8 0 R]] >>",
        ],
    )
}

#[test]
fn tagged_paths_keep_their_mcids_when_styled_and_resized() {
    let (mut a, doc, dir) = open("tagged", tagged());
    let list = call(&mut a, "page_objects", json!({ "doc": doc, "page": 1 }));
    assert_eq!((list["objects"][0]["mcid"].clone(), list["objects"][0]["kind"].clone()), (json!(0), json!("path")));
    for (tool, args) in [
        ("objects_style", json!({ "doc": doc, "page": 1, "objects": [1], "fill": "#ff0000", "opacity": 0.5 })),
        ("objects_resize", json!({ "doc": doc, "page": 1, "objects": [1], "scale": 0.5 })),
    ] {
        let r = call(&mut a, tool, args);
        if let Some(t) = r.get("tags") {
            assert_eq!((t["removed"].as_u64(), t["pruned"].as_u64()), (Some(0), Some(0)), "{tool}: {t}");
            assert!(t["added"].as_array().unwrap().is_empty(), "{tool}: {t}");
        }
    }
    let list = call(&mut a, "page_objects", json!({ "doc": doc, "page": 1 }));
    assert_eq!((list["objects"][0]["mcid"].clone(), list["objects"][1]["mcid"].clone()), (json!(0), json!(1)));
    let types = |a: &mut Automation| -> Vec<String> {
        call(a, "tags_tree", json!({ "doc": doc }))["elements"].as_array().unwrap().iter().map(|e| e["type"].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(types(&mut a), ["Figure", "P"]);
    // Deleting the figure drops its element; the paragraph stays.
    call(&mut a, "objects_delete", json!({ "doc": doc, "page": 1, "objects": [1] }));
    assert_eq!(types(&mut a), ["P"]);
    let list = call(&mut a, "page_objects", json!({ "doc": doc, "page": 1 }));
    assert_eq!((list["count"].clone(), list["objects"][0]["mcid"].clone()), (json!(1), json!(1)));
    let _ = std::fs::remove_dir_all(dir);
}
