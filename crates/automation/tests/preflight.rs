use pdfcraft_automation::{Automation, Content};
use serde_json::{Value, json};

fn call(app: &mut Automation, name: &str, args: Value) -> Value {
    match app.call(name, &args).unwrap().remove(0) {
        Content::Json(v) => v,
        _ => panic!("JSON result"),
    }
}

fn setup(name: &str) -> (Automation, u64, std::path::PathBuf) {
    let directory = std::env::temp_dir().join(format!("pdfcraft-preflight-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let mut app = Automation::new().with_root(&directory).unwrap().with_clock(|| 1_700_000_000);
    let id = call(&mut app, "doc_create", json!({"from":"text","text":"Original synthetic preflight test."}))["doc"].as_u64().unwrap();
    (app, id, directory)
}

#[test]
fn profiles_analysis_and_reports_expose_honest_coverage_and_confined_outputs() {
    let (mut app, doc, dir) = setup("report");
    let dirty_before = app.session().get(pdfcraft_engine::DocId(doc)).unwrap().dirty;
    let catalog = call(&mut app, "preflight_profiles", json!({}));
    assert_eq!(catalog["profiles"].as_array().unwrap().len(), 4);
    let report = call(&mut app, "preflight_analyze", json!({"doc":doc,"profile":"general","checks":["font-embedding"]}));
    assert_eq!(report["validation_complete"], false);
    assert_eq!(report["conformance"], "not-established");
    assert!(report["findings"].as_array().unwrap().iter().all(|f| f["check"] == "font-embedding"));
    assert_eq!(app.session().get(pdfcraft_engine::DocId(doc)).unwrap().dirty, dirty_before);
    for (format, path) in [("text", "report.txt"), ("json", "report.json")] {
        call(&mut app, "preflight_report", json!({"doc":doc,"format":format,"path":path}));
        let content = std::fs::read_to_string(dir.join(path)).unwrap();
        assert!(content.contains(if format == "text" { "does not establish" } else { "not-established" }));
    }
    assert!(app.call("preflight_report", &json!({"doc":doc,"path":"../outside.txt"})).is_err());
    assert!(app.call("preflight_analyze", &json!({"doc":doc,"profile":"pdfx"})).is_err());
    assert!(app.call("preflight_analyze", &json!({"doc":doc,"checks":["invented"]})).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn stale_and_invalid_selected_findings_do_not_create_history() {
    let (mut app, doc, dir) = setup("stale");
    let report = call(&mut app, "preflight_analyze", json!({"doc":doc}));
    let revision = report["revision"].as_u64().unwrap();
    for findings in [json!([]), json!([999999]), json!([1, 1])] {
        assert!(app.call("preflight_fix", &json!({"doc":doc,"revision":revision,"findings":findings})).is_err());
    }
    assert!(app.session().get(pdfcraft_engine::DocId(doc)).unwrap().can_undo().is_none());
    call(&mut app, "doc_set_info", json!({"doc":doc,"key":"Title","value":"Changed"}));
    let before = app.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes.clone();
    assert!(app.call("preflight_fix", &json!({"doc":doc,"revision":revision,"findings":[1]})).unwrap_err().to_string().contains("stale"));
    assert_eq!(before, app.session().get(pdfcraft_engine::DocId(doc)).unwrap().bytes);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn legacy_pdfa_boolean_never_mistakes_an_empty_subset_for_full_conformance() {
    let (mut app, _, dir) = setup("pdfa");
    let doc = call(&mut app, "doc_create", json!({"from":"blank"}))["doc"].as_u64().unwrap();
    let result = call(&mut app, "pdfa_convert", json!({"doc":doc,"level":"2b"}));
    assert_eq!(result["compliant"], false);
    assert_eq!(result["validation_complete"], false);
    assert_eq!(result["conformance"], "not-established");
    assert_eq!(result["no_detected_issues"], result["issues"].as_array().unwrap().is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn headless_selected_fix_is_single_undo_and_preserves_unselected_behavior() {
    let (mut app, _, dir) = setup("fix");
    let blank = pdfcraft_engine::Session::new().create_blank(200.0, 200.0, 1).unwrap();
    let mut cos = pdfcraft_cos::Document::open(blank).unwrap();
    let root = cos.root().unwrap();
    let mut actions = pdfcraft_cos::Dict::new();
    let mut action = pdfcraft_cos::Dict::new();
    action.set(b"S".to_vec(), pdfcraft_cos::Object::name("URI"));
    action.set(b"URI".to_vec(), pdfcraft_cos::PdfString::text("https://example.invalid"));
    actions.set(b"WC".to_vec(), pdfcraft_cos::Object::Dict(action));
    cos.update_dict(root, |d| {
        d.set(b"AA".to_vec(), pdfcraft_cos::Object::Dict(actions));
        d.set(b"Private".to_vec(), pdfcraft_cos::Object::Int(42));
    })
    .unwrap();
    let bytes = pdfcraft_cos::write_full(&cos, &Default::default()).unwrap();
    std::fs::write(dir.join("actions.pdf"), bytes).unwrap();
    let doc = call(&mut app, "doc_open", json!({"path":"actions.pdf"}))["doc"].as_u64().unwrap();
    let report = call(&mut app, "preflight_analyze", json!({"doc":doc}));
    let finding = report["findings"].as_array().unwrap().iter().find(|f| f["check"] == "additional-actions").unwrap()["id"].clone();
    let result = call(&mut app, "preflight_fix", json!({"doc":doc,"revision":report["revision"],"findings":[finding]}));
    assert!(result["findings"].as_array().unwrap().iter().all(|f| f["check"] != "additional-actions"));
    let working = app.session().get(pdfcraft_engine::DocId(doc)).unwrap();
    assert_eq!(working.can_undo(), Some("Apply preflight fixes"));
    let cos = pdfcraft_cos::Document::open(working.bytes.clone()).unwrap();
    assert_eq!(cos.get(root).as_dict().unwrap().int(b"Private"), Some(42));
    call(&mut app, "edit_undo", json!({"doc":doc}));
    let report = call(&mut app, "preflight_analyze", json!({"doc":doc}));
    assert!(report["findings"].as_array().unwrap().iter().any(|f| f["check"] == "additional-actions"));
    assert!(app.session().get(pdfcraft_engine::DocId(doc)).unwrap().can_undo().is_none());
    std::fs::remove_dir_all(dir).unwrap();
}
