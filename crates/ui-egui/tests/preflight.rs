use egui_kittest::{Harness, kittest::Queryable};
use pdfcraft_engine::Session;
use pdfcraft_ui_egui::{Dialog, PdfCraftApp};

#[test]
fn native_preflight_analyzes_closes_and_exports_without_editing() {
    let bytes = Session::new().create_from_text("Synthetic", "A structural preflight test").unwrap().to_vec();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("synthetic.pdf", None, bytes.clone()).unwrap();
        app
    });
    h.run_steps(4);
    assert!(h.state_mut().execute("preflight.run"));
    h.run_steps(2);
    h.get_by_label("Analyze").click();
    h.run_steps(3);
    let id = h.state().active_ids().unwrap().1;
    assert!(h.state().preflight.result.is_some());
    if let Ok(path) = std::env::var("PDFCRAFT_PREFLIGHT_SHOT") {
        h.render().unwrap().save(path).unwrap();
    }
    assert!(!h.state().session.get(id).unwrap().dirty);
    assert!(h.state().session.get(id).unwrap().can_undo().is_none());
    let path = std::env::temp_dir().join(format!("pdfcraft-preflight-ui-{}.txt", std::process::id()));
    h.state_mut().save_override = Some(path.to_string_lossy().into_owned());
    h.get_by_label("Export text report…").click();
    h.run_steps(3);
    assert!(std::fs::read_to_string(&path).unwrap().contains("does not establish"));
    std::fs::remove_file(path).unwrap();
    h.get_by_label("Close").click();
    h.run_steps(2);
    assert_ne!(h.state().dialog, Some(Dialog::Preflight));
    assert!(h.state().session.get(id).unwrap().can_undo().is_none());
}

#[test]
fn native_pdfa_empty_result_does_not_claim_full_conformance() {
    let bytes = Session::new().create_blank(200.0, 200.0, 1).unwrap().to_vec();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("blank.pdf", None, bytes.clone()).unwrap();
        app
    });
    h.run_steps(4);
    h.state_mut().execute("standards.pdfa");
    h.run_steps(2);
    h.get_by_label("Save as PDF/A").click();
    h.run_steps(3);
    h.get_by_label("No issues found in the implemented checks. Full PDF/A conformance is not established.");
    assert!(h.state().toast.as_ref().unwrap().0.contains("not established"));
}

#[test]
fn native_selected_fix_rejects_stale_results_then_supports_undo() {
    use pdfcraft_cos::{Dict, Document, Object};
    let blank = Session::new().create_blank(200.0, 200.0, 1).unwrap();
    let mut cos = Document::open(blank).unwrap();
    let root = cos.root().unwrap();
    cos.update_dict(root, |d| d.set(b"AA".to_vec(), Object::Dict(Dict::new()))).unwrap();
    let bytes = pdfcraft_cos::write_full(&cos, &Default::default()).unwrap();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("actions.pdf", None, bytes.clone()).unwrap();
        app
    });
    h.run_steps(4);
    h.state_mut().execute("preflight.run");
    h.run_steps(2);
    h.get_by_label("Analyze").click();
    h.run_steps(3);
    let id = h.state().active_ids().unwrap().1;
    h.get_by_label("Remove these additional actions (changes interactive behavior)").click();
    h.run_steps(2);
    assert_eq!(h.state().dialog, Some(Dialog::Preflight), "Selecting a fix must not dismiss the dialog");
    h.state_mut().session.apply(id, pdfcraft_engine::Edit::SetInfo { key: "Title".into(), value: "Changed".into() }).unwrap();
    h.get_by_label("Apply selected fixes").click();
    h.run_steps(2);
    assert!(h.state().toast.as_ref().unwrap().0.contains("stale"));
    assert_eq!(h.state().session.get(id).unwrap().can_undo(), Some("Change Title"));
    h.get_by_label("Analyze").click();
    h.run_steps(2);
    h.get_by_label("Remove these additional actions (changes interactive behavior)").click();
    h.run_steps(2);
    h.get_by_label("Apply selected fixes").click();
    h.run_steps(3);
    assert_eq!(h.state().session.get(id).unwrap().can_undo(), Some("Apply preflight fixes"));
    h.state_mut().session.undo(id).unwrap();
    h.get_by_label("Analyze").click();
    h.run_steps(2);
    h.get_by_label("Remove these additional actions (changes interactive behavior)");
}

#[test]
fn failed_analysis_does_not_export_an_earlier_result() {
    let mut app = PdfCraftApp::new();
    let bytes = Session::new().create_blank(200.0, 200.0, 1).unwrap().to_vec();
    app.open_bytes("blank.pdf", None, bytes).unwrap();
    assert!(app.preflight_analyze());
    app.preflight.settings.checks = Some(Vec::new());
    let path = std::env::temp_dir().join(format!("pdfcraft-preflight-failed-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&path);
    app.save_override = Some(path.to_string_lossy().into_owned());
    app.preflight_export_report();
    assert!(app.preflight.result.is_none());
    assert!(!path.exists());
}
