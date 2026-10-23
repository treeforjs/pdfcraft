//! Export a PDF ▸ Image and Text from the real shell (egui_kittest).

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::PdfCraftApp;

const FIXTURE: &[u8] = b"%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 200 100] >> endobj
3 0 obj << /Type /Page /Parent 2 0 R >> endobj
4 0 obj << /Type /Page /Parent 2 0 R >> endobj
trailer << /Root 1 0 R >>
%%EOF";

#[test]
fn export_dialogs_write_images_and_text() {
    let dir = std::env::temp_dir().join(format!("pdfcraft-export-ui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let d = dir.clone();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("doc.pdf", None, FIXTURE.to_vec()).unwrap();
        app.export_dir_override = Some(d.to_string_lossy().into_owned());
        app
    });
    h.run_steps(4);
    assert!(h.state_mut().execute("export.image"));
    h.run_steps(2);
    h.get_by_label("Export to Image");
    h.get_by_label("Export").click();
    h.run_steps(4);
    assert!(dir.join("doc_page_1.png").exists() && dir.join("doc_page_2.png").exists());
    h.get_by_label_contains("Exported 2 images");
    assert!(h.state_mut().execute("export.text"));
    h.run_steps(2);
    h.get_by_label("Export").click();
    h.run_steps(1);
    // Text export runs on a worker thread.
    wait(&mut h, |app| !app.exporting());
    assert!(dir.join("doc.txt").exists());
    // Export all images: this document has none, and says so.
    assert!(h.state_mut().execute("export.all_images"));
    h.run_steps(2);
    h.get_by_label("Export All Images");
    h.get_by_label("Export").click();
    h.run_steps(4);
    h.get_by_label_contains("Exported 0 images");
}

/// Draw frames until `done` holds (the export runs on a worker thread), at most ~10 s.
fn wait(h: &mut Harness<'static, PdfCraftApp>, done: impl Fn(&PdfCraftApp) -> bool) {
    for _ in 0..500 {
        if done(h.state()) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        h.run_steps(1);
    }
    h.run_steps(2);
}

#[test]
fn document_exports_run_in_the_background_from_their_dialogs() {
    let dir = std::env::temp_dir().join(format!("pdfcraft-export-docs-ui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let d = dir.clone();
    let doc = pdfcraft_engine::Session::new().create_from_text("t", "Exported words\n\nA second paragraph").unwrap().to_vec();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("doc.pdf", None, doc).unwrap();
        app.export_dir_override = Some(d.to_string_lossy().into_owned());
        app
    });
    h.run_steps(4);
    for (command, title, file) in [
        ("export.docx", "Export to Word", "doc.docx"),
        ("export.xlsx", "Export to Excel", "doc.xlsx"),
        ("export.pptx", "Export to PowerPoint", "doc.pptx"),
        ("export.html", "Export to HTML", "doc.html"),
        ("export.rtf", "Export to RTF", "doc.rtf"),
        ("export.svg", "Export to SVG", "doc_page_1.svg"),
    ] {
        assert!(h.state_mut().execute(command), "{command}");
        h.run_steps(2);
        h.get_by_label(title);
        h.get_by_label("Export").click();
        h.run_steps(1);
        wait(&mut h, |app| !app.exporting());
        assert!(dir.join(file).exists(), "{command}: {file}");
        h.get_by_label_contains("Exported to");
    }
    let docx = std::fs::read(dir.join("doc.docx")).unwrap();
    assert!(docx.starts_with(b"PK\x03\x04"));
    assert!(std::fs::read_to_string(dir.join("doc.html")).unwrap().contains("A second paragraph"));
    // Accessible text needs tags: this document has none, and the export says so up front.
    assert!(h.state_mut().execute("export.accessible_text"));
    h.run_steps(2);
    h.get_by_label("Export Accessible Text");
    h.get_by_label("Export").click();
    h.run_steps(3);
    assert!(!h.state().exporting());
    h.get_by_label_contains("not tagged");
    assert!(!dir.join("doc.txt").exists());
}

#[test]
fn cancelling_a_document_export_writes_nothing() {
    let dir = std::env::temp_dir().join(format!("pdfcraft-export-cancel-ui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut app = PdfCraftApp::new();
    app.set_option("language", "en").unwrap();
    let text = "A paragraph of words on every page.\n".repeat(12000);
    let doc = pdfcraft_engine::Session::new().create_from_text("long", &text).unwrap().to_vec();
    app.open_bytes("long.pdf", None, doc).unwrap();
    app.export_dir_override = Some(dir.to_string_lossy().into_owned());
    // Cancel straight after starting: the worker stops at its next page, writes nothing and
    // says so.
    assert!(app.execute("export.docx"));
    assert!(app.run_export_dialog());
    assert!(app.exporting());
    app.cancel_export();
    for _ in 0..1000 {
        app.poll_export_for_tests();
        if !app.exporting() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!app.exporting());
    assert!(!dir.join("long.docx").exists());
    assert_eq!(app.toast.clone().map(|t| t.0).as_deref(), Some("Export cancelled"));
}
