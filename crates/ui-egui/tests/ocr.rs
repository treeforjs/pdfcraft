//! Scan & OCR ▸ Recognize text in the real shell (egui_kittest): the dialog, the run and its
//! result (skipped without the OCR models: `cargo xtask models`).

use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use pdfcraft_engine::{Session, export};
use pdfcraft_ui_egui::PdfCraftApp;

/// A one-page PDF that is only a picture of a sentence.
fn scan() -> Vec<u8> {
    let mut s = Session::new();
    let text = s.create_from_text("t", "Recognize these scanned words").unwrap();
    let id = s.open("text.pdf", None, text, None).unwrap();
    let png = export::Exporter::new(s.get(id).unwrap()).png(0, 150.0).unwrap();
    s.create_from_images(&[("scan.png".into(), png)]).unwrap().to_vec()
}

#[test]
fn recognize_text_dialog_adds_searchable_text() {
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(|_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("scan.pdf", None, scan()).unwrap();
        app.run_inline = true;
        app
    });
    h.run_steps(4);
    assert!(h.state_mut().execute("ocr.recognize"));
    h.run_steps(2);
    h.get_by_label("Document language");
    h.get_by_label("Output");
    if !pdfcraft_engine::ocr::Models::required_for_tests().unwrap() {
        eprintln!("skipped: OCR models not installed");
        return;
    }
    h.get_by_label("Recognize text").click();
    h.run_steps(3);
    let id = h.state().active_ids().unwrap().1;
    assert_eq!(h.state().session.get(id).unwrap().can_undo(), None, "recognition waits for review");
    h.get_by_label("Apply recognized text").click();
    h.run_steps(2);
    assert!(h.state().ocr_run.is_none(), "finished");
    let toast = h.state().toast.clone().unwrap().0;
    assert!(toast.starts_with("Recognized ") && toast.contains("on 1 page"), "{toast}");
    let app = h.state();
    let id = app.active_ids().unwrap().1;
    assert_eq!(app.session.get(id).unwrap().can_undo(), Some("Recognize text"));
}

#[test]
fn recognize_text_in_multiple_files_writes_searchable_copies() {
    if !pdfcraft_engine::ocr::Models::required_for_tests().unwrap() {
        eprintln!("skipped: OCR models not installed");
        return;
    }
    let dir = std::env::temp_dir().join(format!("pdfcraft-ocr-ui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut app = PdfCraftApp::new();
    app.set_option("language", "en").unwrap();
    app.run_inline = true;
    app.export_dir_override = Some(dir.to_string_lossy().into_owned());
    app.use_files(pdfcraft_ui_egui::FilePurpose::Ocr, vec![("one.pdf".into(), scan()), ("two.pdf".into(), scan())]);
    assert!(app.ocr_batch.is_none(), "finished");
    let toast = app.toast.clone().unwrap().0;
    assert!(toast.starts_with("Recognized ") && toast.ends_with("in 2 files"), "{toast}");
    for name in ["one.pdf", "two.pdf"] {
        let bytes = std::fs::read(dir.join(name)).unwrap();
        assert!(bytes.len() > scan().len(), "an incremental update was appended");
    }
}

#[test]
fn recognition_review_corrects_words_and_cancel_or_stale_apply_preserves_the_document() {
    if !pdfcraft_engine::ocr::Models::required_for_tests().unwrap() {
        eprintln!("skipped: OCR models not installed");
        return;
    }
    eprintln!("running actual pinned OCR models for review, correction and cancellation in the shell");
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(|_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("scan.pdf", None, scan()).unwrap();
        app.run_inline = true;
        app
    });
    h.run_steps(4);
    let id = h.state().active_ids().unwrap().1;
    let original = h.state().session.get(id).unwrap().bytes.clone();
    for stale in [false, true] {
        h.state_mut().execute("ocr.recognize");
        h.run_steps(2);
        h.get_by_label("Recognize text").click();
        h.run_steps(3);
        h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Recognized word").click();
        h.run_steps(1);
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Recognized word").type_text("café");
        h.run_steps(1);
        // A button loads the next word after the text field was drawn in that frame, so the
        // accessibility tree shows it one frame later.
        h.get_by_label("Next word").click();
        h.run_steps(2);
        assert_ne!(h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Recognized word").accesskit_node().value().as_deref(), Some("café"));
        h.get_by_label("Previous word").click();
        h.run_steps(2);
        assert_eq!(h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Recognized word").accesskit_node().value().as_deref(), Some("café"));
        assert_eq!(h.state().session.get(id).unwrap().bytes, original);
        assert_eq!(h.state().session.get(id).unwrap().can_undo(), None);
        if stale {
            h.state_mut().session.apply(id, pdfcraft_engine::Edit::SetInfo { key: "Title".into(), value: "Changed".into() }).unwrap();
            h.run_steps(1);
            assert!(h.get_by_label("Apply recognized text").accesskit_node().is_disabled());
            h.get_by_label_contains("document changed after recognition started");
        }
        let before = h.state().session.get(id).unwrap().bytes.clone();
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        assert!(h.state().ocr_run.is_none());
        assert_eq!(h.state().session.get(id).unwrap().bytes, before);
    }
}
