//! The Edit object tool headless: `objects_select` (click and marquee), `page_objects`'
//! appearance and opacity, `objects_transform` by any angle and `objects_style` opacity on text,
//! end to end through the tool table, with undo, save and reopen, and hostile arguments.

use pdfcraft_automation::{Automation, Content, ToolError};
use serde_json::{Value, json};

fn call(app: &mut Automation, name: &str, args: Value) -> Value {
    match app.call(name, &args).unwrap_or_else(|e| panic!("{name}: {e:?}")).remove(0) {
        Content::Json(v) => v,
        _ => panic!("JSON result"),
    }
}

/// A 200×200 page: a red square, a blue stroked square over it, and a line of text.
fn pdf(content: &str) -> Vec<u8> {
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 200] >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
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

const PAGE: &str = "1 0 0 rg 20 120 60 60 re f\n0 0 1 RG 4 w 50 100 60 60 re S\nBT /F1 12 Tf 20 20 Td (Hello) Tj ET";

fn open(name: &str) -> (Automation, u64, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("pdfcraft-edit-object-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("page.pdf"), pdf(PAGE)).unwrap();
    let mut app = Automation::new().with_root(&dir).unwrap();
    let doc = call(&mut app, "doc_open", json!({"path":"page.pdf"}))["doc"].as_u64().unwrap();
    (app, doc, dir)
}

fn objects(a: &mut Automation, doc: u64) -> Vec<Value> {
    call(a, "page_objects", json!({ "doc": doc, "page": 1 }))["objects"].as_array().unwrap().clone()
}

#[test]
fn objects_are_picked_by_click_and_marquee_and_report_their_look() {
    let (mut a, doc, dir) = open("select");
    let list = objects(&mut a, doc);
    assert_eq!(list.len(), 3);
    assert_eq!(list[0]["appearance"]["fill"], json!({ "space": "rgb", "components": [1.0, 0.0, 0.0] }));
    assert_eq!(list[0]["appearance"]["stroke"], "none");
    assert_eq!(list[1]["appearance"]["stroke"], json!({ "space": "rgb", "components": [0.0, 0.0, 1.0] }));
    assert_eq!(list[1]["appearance"]["line_width"], 4.0);
    assert_eq!((list[2]["kind"].as_str(), list[2]["opacity"].as_f64()), (Some("text"), Some(1.0)));
    assert!(list[2].get("appearance").is_none());

    // A click where the squares overlap picks the stroked one on top (view y runs down).
    let hit = call(&mut a, "objects_select", json!({ "doc": doc, "page": 1, "point": [60, 60] }));
    assert_eq!((hit["top"].clone(), hit["objects"].clone()), (json!(2), json!([2, 1])));
    let miss = call(&mut a, "objects_select", json!({ "doc": doc, "page": 1, "point": [190, 190], "tolerance": 0 }));
    assert_eq!((miss["count"].clone(), miss["top"].clone()), (json!(0), Value::Null));
    let marquee = call(&mut a, "objects_select", json!({ "doc": doc, "page": 1, "rect": [0, 0, 200, 90] }));
    assert_eq!(marquee["objects"], json!([1, 2]));
    let text = call(&mut a, "objects_select", json!({ "doc": doc, "page": 1, "rect": [10, 200, 40, 170] }));
    assert_eq!(text["objects"], json!([3]));

    for args in [
        json!({ "doc": doc, "page": 1 }),
        json!({ "doc": doc, "page": 1, "point": [1, 2], "rect": [0, 0, 1, 1] }),
        json!({ "doc": doc, "page": 1, "point": [1e300, 0] }),
        json!({ "doc": doc, "page": 1, "point": [1] }),
        json!({ "doc": doc, "page": 1, "point": ["a", 1] }),
        json!({ "doc": doc, "page": 1, "point": [1, 1], "tolerance": -1 }),
        json!({ "doc": doc, "page": 1, "point": [1, 1], "tolerance": 1e9 }),
        json!({ "doc": doc, "page": 1, "rect": [0, 0, 1e300, 1] }),
        json!({ "doc": doc, "page": 9, "point": [1, 1] }),
    ] {
        assert!(matches!(a.call("objects_select", &args), Err(ToolError::InvalidArgs(_) | ToolError::Failed(_))), "{args}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn objects_turn_by_any_angle_and_take_opacity_in_one_undo_step() {
    let (mut a, doc, dir) = open("transform");
    let before = objects(&mut a, doc);
    let r = call(&mut a, "objects_transform", json!({ "doc": doc, "page": 1, "objects": [1], "action": "rotate", "degrees": 45 }));
    assert_eq!(r["undo"], "Rotate objects");
    let after = objects(&mut a, doc);
    let rect: Vec<f64> = after[0]["rect"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    let half = 30.0 * 2f64.sqrt();
    for (v, want) in rect.iter().zip([50.0 - half, 50.0 - half, 50.0 + half, 50.0 + half]) {
        assert!((v - want).abs() < 0.02, "{rect:?}");
    }
    assert_eq!(after[1..], before[1..], "the others are untouched");
    call(&mut a, "edit_undo", json!({ "doc": doc }));
    assert_eq!(objects(&mut a, doc), before, "one undo step");

    let r = call(&mut a, "objects_style", json!({ "doc": doc, "page": 1, "objects": [3], "opacity": 0.5 }));
    assert_eq!(r["undo"], "Change path appearance");
    assert_eq!(objects(&mut a, doc)[2]["opacity"], 0.5);
    assert_eq!(objects(&mut a, doc)[2]["kind"], "text");
    call(&mut a, "doc_save", json!({ "doc": doc, "path": "out.pdf" }));
    let re = call(&mut a, "doc_open", json!({ "path": "out.pdf" }))["doc"].as_u64().unwrap();
    assert_eq!(objects(&mut a, re)[2]["opacity"], 0.5);

    // A view-space matrix scales both squares together about the view origin (y down).
    let r =
        call(&mut a, "objects_transform", json!({ "doc": doc, "page": 1, "objects": [1, 2], "action": "matrix", "matrix": [0.5, 0, 0, 0.5, 0, 0] }));
    assert_eq!(r["undo"], "Resize objects");
    let list = objects(&mut a, doc);
    assert_eq!(list[0]["rect"], json!([10.0, 10.0, 40.0, 40.0]));
    assert_eq!(list[1]["rect"], json!([24.0, 19.0, 56.0, 51.0]), "the 4 pt stroke halves too");
    call(&mut a, "edit_undo", json!({ "doc": doc }));
    let turned =
        call(&mut a, "objects_transform", json!({ "doc": doc, "page": 1, "objects": [1, 2], "action": "matrix", "matrix": [0, 1, -1, 0, 200, 0] }));
    assert_eq!(turned["undo"], "Transform objects");
    call(&mut a, "edit_undo", json!({ "doc": doc }));

    for (tool, args) in [
        ("objects_transform", json!({ "doc": doc, "page": 1, "objects": [1], "action": "matrix" })),
        ("objects_transform", json!({ "doc": doc, "page": 1, "objects": [1], "action": "matrix", "matrix": [1, 0, 0, 1] })),
        ("objects_transform", json!({ "doc": doc, "page": 1, "objects": [1], "action": "matrix", "matrix": [0, 0, 0, 0, 0, 0] })),
        ("objects_transform", json!({ "doc": doc, "page": 1, "objects": [1], "action": "matrix", "matrix": [1e300, 0, 0, 1, 0, 0] })),
        ("objects_transform", json!({ "doc": doc, "page": 1, "objects": [1], "action": "matrix", "matrix": [1, 0, 0, 1, 1e12, 0] })),
        ("objects_transform", json!({ "doc": doc, "page": 1, "objects": [1], "action": "rotate", "degrees": 1e9 })),
        ("objects_transform", json!({ "doc": doc, "page": 1, "objects": [1], "action": "rotate", "degrees": 10, "quarters": 1 })),
        ("objects_transform", json!({ "doc": doc, "page": 1, "objects": [1], "action": "rotate", "degrees": "x" })),
        ("objects_transform", json!({ "doc": doc, "page": 1, "objects": [0], "action": "rotate", "degrees": 10 })),
        ("objects_transform", json!({ "doc": doc, "page": 1, "objects": [4], "action": "move", "dx": 1 })),
        ("objects_transform", json!({ "doc": doc, "page": 1, "objects": [1], "action": "move", "dx": 1e300 })),
        ("objects_style", json!({ "doc": doc, "page": 1, "objects": [3], "fill": "#000000" })),
        ("objects_style", json!({ "doc": doc, "page": 1, "objects": [3], "opacity": -0.5 })),
    ] {
        assert!(matches!(a.call(tool, &args), Err(ToolError::InvalidArgs(_) | ToolError::Failed(_))), "{tool} {args}");
    }
    let _ = std::fs::remove_dir_all(dir);
}
