//! Prepare for accessibility ▸ Check for accessibility in the real shell (egui_kittest): the
//! options dialog, the results panel, Fix, Skip Rule and the report.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use printcraft_engine::a11y::{Rule, Status};
use printcraft_ui_egui::{Dialog, PrintCraftApp, RightPanel};

const FIXTURE: &[u8] = b"%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 100] >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >> endobj
4 0 obj << /Length 37 >> stream
BT /F1 12 Tf 20 50 Td (Hello) Tj ET
endstream endobj
5 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
trailer << /Root 1 0 R >>
%%EOF";

fn status(h: &Harness<'static, PrintCraftApp>, rule: Rule) -> Status {
    h.state().a11y.report.as_ref().unwrap().1.result(rule).unwrap().status
}

fn right_click(h: &mut Harness<'static, PrintCraftApp>, label: &str) {
    let at = h.get_by_label(label).rect().center();
    h.hover_at(at);
    h.run_steps(1);
    h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Secondary, pressed: true, modifiers: Default::default() });
    h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Secondary, pressed: false, modifiers: Default::default() });
    h.run_steps(2);
}

#[test]
fn checking_fixing_skipping_and_reporting() {
    let dir = std::env::temp_dir().join(format!("printcraft-a11y-ui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let d = dir.clone();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PrintCraftApp::new();
        app.open_bytes("notes.pdf", None, FIXTURE.to_vec()).unwrap();
        app.export_dir_override = Some(d.to_string_lossy().into_owned());
        app
    });
    h.run_steps(4);
    assert!(h.state_mut().execute("a11y.check"));
    h.run_steps(2);
    h.get_by_label("Accessibility Checker Options");
    h.get_by_label_contains("31 of 32");
    h.get_by_label("Document title is showing in title bar");
    h.get_by_label("Start Checking").click();
    h.run_steps(3);
    assert_eq!(h.state().dialog, None);
    assert_eq!(h.state().right, Some(RightPanel::Accessibility));
    h.get_by_label("Document (3 issues)");
    h.get_by_label("Title - Failed");
    h.get_by_label("Color contrast - Skipped");
    // Fix the title: the file name becomes the title and it shows; the description opens to edit it.
    right_click(&mut h, "Title - Failed");
    h.get_by_label("Fix").click();
    h.run_steps(3);
    assert_eq!(status(&h, Rule::Title), Status::Passed);
    assert!(matches!(h.state().dialog, Some(Dialog::Properties(_))));
    assert_eq!(h.state().window_title, "notes — PrintCraft");
    h.state_mut().dialog = None;
    h.run_steps(2);
    // Skip a rule; it stays skipped when checking again.
    right_click(&mut h, "Tagged PDF - Failed");
    h.get_by_label("Skip Rule").click();
    h.run_steps(2);
    h.get_by_label("Check again").click();
    h.run_steps(3);
    assert_eq!(status(&h, Rule::TaggedPdf), Status::Skipped);
    h.get_by_label("Document (1 issue)");
    // Explain shows why the rule matters; findings list the problems.
    right_click(&mut h, "Primary language - Failed");
    h.get_by_label("Explain").click();
    h.run_steps(2);
    h.get_by_label_contains("Screen readers choose their voice");
    h.get_by_label("Primary language - Failed").click();
    h.run_steps(2);
    h.get_by_label("No document language is set");
    // The report.
    h.get_by_label("Report").click();
    h.run_steps(2);
    let html = std::fs::read_to_string(dir.join("notes Accessibility Report.html")).unwrap();
    assert!(html.contains("Accessibility Report") && html.contains("Skipped"));
}
