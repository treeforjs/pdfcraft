//! `doc_export_svg` end to end: synthetic pages through the tool table, output parsed as XML.

use pdfcraft_automation::{Automation, Content, ToolError};
use serde_json::{Value, json};

fn call(app: &mut Automation, name: &str, args: Value) -> Value {
    match app.call(name, &args).unwrap_or_else(|e| panic!("{name} {args}: {e}")).remove(0) {
        Content::Json(v) => v,
        _ => panic!("JSON result"),
    }
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut o = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    o.extend_from_slice(data);
    o.extend_from_slice(b"\nendstream");
    o
}

/// Three 200 × 100 pt pages: vector art and text; a page turned 90° showing an image; a form
/// that draws itself.
fn fixture() -> Vec<u8> {
    let page = |contents: usize, extra: &str| {
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents {contents} 0 R /Resources 9 0 R {extra} >>").into_bytes()
    };
    let objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>".to_vec(),
        page(6, ""),
        page(7, "/Rotate 90"),
        page(8, ""),
        stream("", b"1 0 0 rg 10 10 50 50 re f 0 0 1 RG 3 w [4 2] 0 d 10 80 m 190 80 l S BT /F1 18 Tf 80 30 Td (Hello) Tj ET"),
        stream("", b"q 100 0 0 50 10 20 cm /Im1 Do Q"),
        stream("", b"/Loop Do"),
        b"<< /Font << /F1 10 0 R >> /XObject << /Im1 11 0 R /Loop 12 0 R >> >>".to_vec(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        stream("/Type /XObject /Subtype /Image /Width 2 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8", &[255, 0, 0, 0, 0, 255]),
        stream("/Type /XObject /Subtype /Form /BBox [0 0 200 100] /Resources 9 0 R", b"0 1 0 rg 0 0 5 5 re f /Loop Do"),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
    out
}

fn setup(name: &str) -> (Automation, u64, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("pdfcraft-svg-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("art.pdf"), fixture()).unwrap();
    let mut app = Automation::new().with_root(&dir).unwrap();
    let doc = call(&mut app, "doc_open", json!({"path":"art.pdf"}))["doc"].as_u64().unwrap();
    (app, doc, dir)
}

fn named<'a>(doc: &'a roxmltree::Document<'a>, name: &str) -> Vec<roxmltree::Node<'a, 'a>> {
    doc.descendants().filter(|n| n.tag_name().name() == name).collect()
}

const XLINK: &str = "http://www.w3.org/1999/xlink";

#[test]
fn exporting_pages_to_svg_through_tools() {
    let (mut app, doc, dir) = setup("pages");
    let root = dir.canonicalize().unwrap();
    let out = call(&mut app, "doc_export_svg", json!({"doc":doc,"folder":"svg"}));
    assert_eq!(out["count"], 3, "{out}");
    let files = out["files"].as_array().unwrap();
    let texts: Vec<String> = files
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let path = f["path"].as_str().unwrap();
            assert!(path.ends_with(&format!("art_page_{}.svg", i + 1)), "{path}");
            let actual = std::path::Path::new(path).canonicalize().expect("the exported SVG exists");
            assert!(actual.starts_with(&root), "inside the root: {path}");
            assert_eq!(actual, root.join("svg").join(format!("art_page_{}.svg", i + 1)));
            assert_eq!(f["truncated"], false);
            std::fs::read_to_string(actual).unwrap()
        })
        .collect();

    // Page 1: vector paths with their colours and dashes, text as outlines plus a text layer.
    let page = roxmltree::Document::parse(&texts[0]).unwrap();
    assert_eq!(page.root_element().attribute("viewBox"), Some("0 0 200 100"));
    let paths = named(&page, "path");
    let red = paths.iter().find(|p| p.attribute("fill") == Some("#ff0000")).unwrap();
    assert_eq!(red.attribute("d"), Some("M10 90 L60 90 L60 40 L10 40 Z"));
    let rule = paths.iter().find(|p| p.attribute("stroke") == Some("#0000ff")).unwrap();
    assert_eq!((rule.attribute("stroke-width"), rule.attribute("stroke-dasharray")), (Some("3"), Some("4 2")));
    let glyphs = named(&page, "use");
    assert_eq!(glyphs.len(), 5, "H e l l o");
    assert!(glyphs.iter().all(|g| g.attribute((XLINK, "href")).is_some_and(|h| h.starts_with("#g"))));
    assert_eq!(files[0]["glyphs"], 5);
    assert_eq!(named(&page, "text")[0].text(), Some("Hello"));

    // Page 2: the rotated page's displayed box, and the image embedded as PNG.
    let page = roxmltree::Document::parse(&texts[1]).unwrap();
    assert_eq!(page.root_element().attribute("viewBox"), Some("0 0 100 200"));
    assert_eq!((files[1]["width"].as_f64(), files[1]["height"].as_f64()), (Some(100.0), Some(200.0)));
    let image = named(&page, "image").into_iter().next().unwrap();
    // "iVBORw0KGgo" is the PNG signature in base64.
    assert!(image.attribute((XLINK, "href")).unwrap().starts_with("data:image/png;base64,iVBORw0KGgo"));
    assert_eq!(files[1]["images"], 1);

    // Page 3: the form that draws itself ends at the interpreter's nesting limit.
    let page = roxmltree::Document::parse(&texts[2]).unwrap();
    let green = named(&page, "path").into_iter().filter(|p| p.attribute("fill") == Some("#00ff00")).count();
    assert!((1..=64).contains(&green), "{green}");

    // A page range, no text layer, transparent paper.
    let out = call(&mut app, "doc_export_svg", json!({"doc":doc,"folder":"one","pages":[1],"text_layer":false,"transparent":true}));
    assert_eq!(out["count"], 1);
    let text = std::fs::read_to_string(dir.join("one/art_page_1.svg")).unwrap();
    let page = roxmltree::Document::parse(&text).unwrap();
    assert!(named(&page, "text").is_empty() && named(&page, "rect").is_empty());
    assert_eq!(named(&page, "use").len(), 5, "the outlines stay");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn svg_export_includes_unsaved_edits() {
    let (mut app, doc, dir) = setup("edits");
    call(&mut app, "page_rotate", json!({"doc":doc,"pages":[1],"degrees":90}));
    let out = call(&mut app, "doc_export_svg", json!({"doc":doc,"folder":"svg","pages":[1]}));
    let text = std::fs::read_to_string(out["files"][0]["path"].as_str().unwrap()).unwrap();
    assert!(text.contains("viewBox=\"0 0 100 200\""));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn svg_export_rejects_bad_arguments() {
    let (mut app, doc, dir) = setup("bad");
    let invalid = |app: &mut Automation, args: Value| match app.call("doc_export_svg", &args) {
        Err(ToolError::InvalidArgs(_)) => {}
        other => panic!("{args}: {other:?}"),
    };
    invalid(&mut app, json!({"doc":doc}));
    invalid(&mut app, json!({"doc":doc,"folder":"x","pages":[9]}));
    invalid(&mut app, json!({"doc":doc,"folder":"x","pages":[0]}));
    invalid(&mut app, json!({"doc":doc,"folder":"x","text_layer":"yes"}));
    assert!(app.call("doc_export_svg", &json!({"doc":doc,"folder":"../outside"})).is_err());
    assert!(app.call("doc_export_svg", &json!({"doc":999,"folder":"x"})).is_err());
    assert!(!dir.join("x").join("art_page_1.svg").exists());
    let _ = std::fs::remove_dir_all(dir);
}
