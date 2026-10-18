//! `print_object_inspector` end to end through the tool table: stacked objects at a point with
//! known colours, overprint, transparency, an image with a known resolution, text with a known
//! font, the ink values there, Ink Manager routing and bad arguments.

use pdfcraft_automation::{Automation, Content};
use serde_json::{Value, json};

fn call(app: &mut Automation, name: &str, args: Value) -> Value {
    match app.call(name, &args).unwrap().remove(0) {
        Content::Json(v) => v,
        _ => panic!("JSON result"),
    }
}

fn stream(extra: &str, data: &[u8]) -> Vec<u8> {
    let mut out = format!("<< /Length {} {extra} >>\nstream\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream");
    out
}

const RESOURCES: &str = "<< /ColorSpace << \
    /Gold [/Separation /Gold /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0.2 1 0] /N 1 >>] >> \
    /ExtGState << /OP << /OP true /op true /OPM 1 >> /Soft << /ca 0.5 /BM /Screen >> >> \
    /Font << /F1 5 0 R >> /XObject << /Im1 7 0 R >> >>";

/// A 200 × 100 pt page (rotated by `rotate`): a CMYK base square, a Gold square over it, a
/// cyan overprinting stroke on top; a half-transparent black square; a 30 × 15 px image drawn
/// 1 × 0.5 inch (30 ppi); text in an embedded subset font.
fn open(name: &str, rotate: u32) -> (Automation, u64, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("pdfcraft-inspector-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let content: &[u8] = b"0.1 0.2 0.3 0.4 k 0 0 60 60 re f /Gold cs 0.5 scn 10 10 40 40 re f \
        q /OP gs 1 0 0 0 K 2 w 30 0 m 30 60 l S Q \
        q /Soft gs 0 0 0 1 k 70 0 20 20 re f Q \
        q 72 0 0 36 100 10 cm /Im1 Do Q \
        BT 0 g /F1 10 Tf 100 70 Td (Text) Tj ET";
    let objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Rotate {rotate} /Resources {RESOURCES} /Contents 4 0 R >>").into_bytes(),
        stream("", content),
        b"<< /Type /Font /Subtype /TrueType /BaseFont /ABCDEF+Demo /FirstChar 32 /LastChar 126 /FontDescriptor 6 0 R >>".to_vec(),
        b"<< /Type /FontDescriptor /FontName /ABCDEF+Demo /Flags 32 /Ascent 800 /Descent -200 /FontFile2 8 0 R >>".to_vec(),
        stream("/Type /XObject /Subtype /Image /Width 30 /Height 15 /BitsPerComponent 8 /ColorSpace /DeviceCMYK", &[0u8; 30 * 15 * 4]),
        stream("", b"not a real font"),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offs = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let x = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offs {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n", objs.len() + 1).as_bytes());
    std::fs::write(dir.join("inspect.pdf"), out).unwrap();
    let mut app = Automation::new().with_root(&dir).unwrap();
    let doc = call(&mut app, "doc_open", json!({"path":"inspect.pdf"}))["doc"].as_u64().unwrap();
    (app, doc, dir)
}

fn ink(list: &Value, name: &str) -> f64 {
    list.as_array().unwrap().iter().find(|i| i["name"] == name).unwrap_or_else(|| panic!("no {name} in {list}"))["percent"].as_f64().unwrap()
}

#[test]
fn stacked_objects_list_topmost_first_with_colours_overprint_and_inks() {
    let (mut app, doc, dir) = open("stack", 0);
    // (30, 70) from the top-left is user (30, 30): stroke over Gold over CMYK.
    let r = call(&mut app, "print_object_inspector", json!({"doc": doc, "page": 1, "x": 30, "y": 70}));
    assert_eq!(r["count"], 3, "{r}");
    let objects = r["objects"].as_array().unwrap();
    assert!(objects.iter().all(|o| o["type"] == "path"));
    let top = &objects[0];
    assert_eq!(top["painted"], json!(["stroke"]));
    assert_eq!(top["stroke"]["space"], "DeviceCMYK");
    assert_eq!(ink(&top["stroke"]["inks"], "Cyan"), 100.0);
    assert_eq!(top["overprint"], json!({"fill": true, "stroke": true, "mode": 1, "op": true, "OP": true, "OPM": 1}));
    assert_eq!(top["line_width"], 2.0);
    assert_eq!(objects[1]["fill"]["space"], "Separation (Gold)");
    assert_eq!(ink(&objects[1]["fill"]["inks"], "Gold"), 50.0);
    assert_eq!(objects[1]["overprint"]["op"], false);
    assert_eq!(ink(&objects[2]["fill"]["inks"], "Black"), 40.0);
    assert_eq!(objects[2]["rect"], json!([0.0, 40.0, 60.0, 100.0]));
    assert_eq!(objects[2]["pdf_rect"], json!([0.0, 0.0, 60.0, 60.0]));
    // The plates there: the overprinting cyan keeps Gold and the CMYK's magenta, yellow, black.
    let seps = &r["separations"]["inks"];
    assert_eq!(ink(seps, "Cyan"), 100.0);
    assert_eq!(ink(seps, "Gold"), 50.0);
    assert_eq!(ink(seps, "Black"), 0.0);
    // Without overprint the stroke knocks out everything else.
    let k = call(&mut app, "print_object_inspector", json!({"doc": doc, "page": 1, "x": 30, "y": 70, "overprint": false}));
    assert_eq!(ink(&k["separations"]["inks"], "Gold"), 0.0);
    assert_eq!(k["overprint_simulated"], false);
    // Off every object: nothing.
    let none = call(&mut app, "print_object_inspector", json!({"doc": doc, "page": 1, "x": 195, "y": 95}));
    assert_eq!(none["count"], 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn transparency_images_and_text_are_described() {
    let (mut app, doc, dir) = open("details", 0);
    let soft = call(&mut app, "print_object_inspector", json!({"doc": doc, "page": 1, "x": 80, "y": 10, "units": "pdf"}));
    let t = &soft["objects"][0]["transparency"];
    assert_eq!((t["transparent"].as_bool(), t["blend_mode"].as_str(), t["fill_alpha"].as_f64()), (Some(true), Some("Screen"), Some(0.5)));
    assert_eq!(soft["pdf_point"], json!([80.0, 10.0]));
    assert_eq!(soft["point"], json!([80.0, 90.0]));
    let image = call(&mut app, "print_object_inspector", json!({"doc": doc, "page": 1, "x": 120, "y": 20, "units": "pdf"}));
    let im = &image["objects"][0];
    assert_eq!(im["type"], "image");
    assert_eq!(im["image"]["name"], "Im1");
    assert_eq!(
        (im["image"]["width"].as_i64(), im["image"]["height"].as_i64(), im["image"]["bits_per_component"].as_i64()),
        (Some(30), Some(15), Some(8))
    );
    assert_eq!(im["image"]["colour_space"], "DeviceCMYK");
    assert_eq!(im["image"]["ppi"], json!([30.0, 30.0]));
    assert_eq!(im["image"]["effective_ppi"], 30.0);
    let text = call(&mut app, "print_object_inspector", json!({"doc": doc, "page": 1, "x": 105, "y": 73, "units": "pdf"}));
    let tx = &text["objects"][0];
    assert_eq!(tx["type"], "text");
    assert_eq!(tx["text"]["font"], "ABCDEF+Demo");
    assert_eq!(tx["text"]["font_type"], "TrueType");
    assert_eq!(tx["text"]["embedded"], true);
    assert_eq!(tx["text"]["subset"], true);
    assert_eq!(tx["text"]["size"], 10.0);
    assert_eq!(tx["fill"]["space"], "DeviceGray");
    assert_eq!(ink(&tx["fill"]["inks"], "Black"), 100.0);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn rotated_pages_and_the_ink_manager_are_honoured() {
    let (mut app, doc, dir) = open("rotated", 90);
    // Rotated 90° clockwise the page shows 100 × 200; user (20, 30) is displayed at (30, 20).
    let r = call(&mut app, "print_object_inspector", json!({"doc": doc, "page": 1, "x": 30, "y": 20}));
    assert_eq!(r["pdf_point"], json!([20.0, 30.0]));
    assert_eq!(r["objects"][0]["fill"]["space"], "Separation (Gold)");
    call(&mut app, "print_ink_manager_set", json!({"doc": doc, "aliases": {"Gold": "Yellow"}}));
    let routed = call(&mut app, "print_object_inspector", json!({"doc": doc, "page": 1, "x": 30, "y": 20}));
    assert_eq!(ink(&routed["objects"][0]["fill"]["inks"], "Yellow"), 50.0);
    assert_eq!(routed["routed"]["Gold"], "Yellow");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn bad_arguments_are_refused() {
    let (mut app, doc, dir) = open("bad", 0);
    for args in [
        json!({"doc": doc, "page": 1, "x": 30}),
        json!({"doc": doc, "page": 1, "x": 300, "y": 10}),
        json!({"doc": doc, "page": 1, "x": 10, "y": 10, "units": "mm"}),
        json!({"doc": doc, "page": 1, "x": 10, "y": 10, "dpi": 1}),
        json!({"doc": doc, "page": 9, "x": 10, "y": 10}),
        json!({"doc": doc, "page": 1, "x": "a", "y": 10}),
    ] {
        assert!(app.call("print_object_inspector", &args).is_err(), "{args}");
    }
    // A point outside the crop box in pdf units still answers (nothing there).
    let r = call(&mut app, "print_object_inspector", json!({"doc": doc, "page": 1, "x": -50, "y": 1e9, "units": "pdf"}));
    assert_eq!(r["count"], 0);
    assert!(r["separations"].is_null());
    let _ = std::fs::remove_dir_all(dir);
}
