//! `print_total_area_coverage` end to end: synthetic pages through the tool table.

use pdfcraft_automation::{Automation, Content};
use pdfcraft_cos::{Dict, Document, Object, Stream};
use serde_json::{Value, json};

fn call(app: &mut Automation, name: &str, args: Value) -> Value {
    match app.call(name, &args).unwrap().remove(0) {
        Content::Json(v) => v,
        _ => panic!("JSON result"),
    }
}

/// A temp root holding `name` = one page of `size` points whose content is `content`.
fn open(name: &str, size: [f64; 2], content: &[u8]) -> (Automation, u64, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("pdfcraft-coverage-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let blank = pdfcraft_engine::Session::new().create_blank(200.0, 200.0, 1).unwrap();
    let mut cos = Document::open(blank).unwrap();
    let page = pdfcraft_model::pages(&cos)[0].obj;
    let stream = cos.add(Object::Stream(Stream::from_raw(Dict::new(), content.to_vec())));
    cos.update_dict(page, |d| {
        d.set(b"Contents".to_vec(), Object::Ref(stream));
        d.set(b"MediaBox".to_vec(), Object::Array(vec![Object::Int(0), Object::Int(0), Object::Real(size[0]), Object::Real(size[1])]));
    })
    .unwrap();
    std::fs::write(dir.join("page.pdf"), pdfcraft_cos::write_full(&cos, &Default::default()).unwrap()).unwrap();
    let mut app = Automation::new().with_root(&dir).unwrap();
    let doc = call(&mut app, "doc_open", json!({"path":"page.pdf"}))["doc"].as_u64().unwrap();
    (app, doc, dir)
}

#[test]
fn rich_black_is_flagged_with_regions_in_tool_coordinates() {
    let (mut app, doc, dir) = open("rich", [200.0, 200.0], b"1 1 1 1 k 20 120 60 60 re f");
    let out = call(&mut app, "print_total_area_coverage", json!({"doc":doc}));
    assert_eq!(out["flagged_pages"], json!([1]));
    let page = &out["pages"][0];
    assert_eq!(page["max"], 400.0);
    assert_eq!(page["flagged"], true);
    assert!((page["over_area"].as_f64().unwrap() - 3600.0).abs() < 1.0);
    let rect: Vec<f64> = page["regions"][0]["rect"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    // Top-left origin: the box spans y 120–180 in user space, 20–80 from the top.
    for (got, want) in rect.iter().zip([20.0, 20.0, 80.0, 80.0]) {
        assert!((got - want).abs() <= 1.0, "{rect:?}");
    }
    assert_eq!(page["pdf_rect"], Value::Null);
    assert_eq!(page["regions"][0]["pdf_rect"][1], 120.0);
    assert!(out["method"].as_str().unwrap().contains("RGB"));
    assert!(!app.session().get(pdfcraft_engine::DocId(doc)).unwrap().dirty);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn black_text_page_is_not_flagged() {
    let dir = std::env::temp_dir().join(format!("pdfcraft-coverage-text-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut app = Automation::new().with_root(&dir).unwrap();
    let doc = call(&mut app, "doc_create", json!({"from":"text","text":"Plain black body text for a coverage check."}))["doc"].as_u64().unwrap();
    let out = call(&mut app, "print_total_area_coverage", json!({"doc":doc}));
    assert_eq!(out["flagged_pages"], json!([]));
    let page = &out["pages"][0];
    assert_eq!(page["flagged"], false);
    assert!(page["max"].as_f64().unwrap() <= 100.0);
    assert!(page["max"].as_f64().unwrap() > 0.0, "the text was measured");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn the_limit_and_region_count_are_parameters() {
    let (mut app, doc, dir) = open("limit", [200.0, 200.0], b"0.7 0.7 0.7 0.7 k 0 0 20 20 re f 50 50 20 20 re f 100 100 20 20 re f");
    assert_eq!(call(&mut app, "print_total_area_coverage", json!({"doc":doc}))["flagged_pages"], json!([]));
    let out = call(&mut app, "print_total_area_coverage", json!({"doc":doc,"limit":250,"max_regions":2,"pages":[1],"dpi":36}));
    assert_eq!(out["flagged_pages"], json!([1]));
    assert_eq!(out["pages"][0]["region_count"], 3);
    assert_eq!(out["pages"][0]["regions"].as_array().unwrap().len(), 2);
    assert_eq!(out["pages"][0]["dpi"], 36.0);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn bad_arguments_are_rejected() {
    let (mut app, doc, dir) = open("bad", [200.0, 200.0], b"");
    for args in [
        json!({"doc":doc,"limit":0}),
        json!({"doc":doc,"limit":"300"}),
        json!({"doc":doc,"dpi":1000}),
        json!({"doc":doc,"dpi":2}),
        json!({"doc":doc,"max_regions":0}),
        json!({"doc":doc,"max_regions":-1}),
        json!({"doc":doc,"pages":[2]}),
        json!({"doc":doc,"pages":[]}),
        json!({"doc":99}),
    ] {
        assert!(app.call("print_total_area_coverage", &args).is_err(), "{args}");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn huge_pages_are_measured_at_a_capped_resolution() {
    let (mut app, doc, dir) = open("huge", [14400.0, 14400.0], b"1 1 1 1 k 0 0 14400 14400 re f");
    let out = call(&mut app, "print_total_area_coverage", json!({"doc":doc,"dpi":300}));
    let page = &out["pages"][0];
    assert!(page["dpi"].as_f64().unwrap() < 300.0);
    assert_eq!(page["over_percent"], 100.0);
    assert_eq!(page["truncated"], false);
    std::fs::remove_dir_all(dir).unwrap();
}
