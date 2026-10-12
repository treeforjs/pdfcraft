//! Compare files in the real shell (egui_kittest): the dialog, the Compare panel, marks and
//! the report.

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use pdfcraft_engine::Session;
use pdfcraft_ui_egui::{PdfCraftApp, RightPanel};

#[test]
fn compare_two_versions() {
    let s = Session::new();
    let v1 = s.create_from_text("t", "Delivery within five days. Returns accepted.").unwrap().to_vec();
    let v2 = s.create_from_text("t", "Delivery within three days. Returns accepted for a week.").unwrap().to_vec();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("v1.pdf", None, v1.clone()).unwrap();
        app.open_bytes("v2.pdf", None, v2.clone()).unwrap();
        app
    });
    h.run_steps(4);
    assert!(h.state_mut().execute("doc.compare"));
    h.run_steps(2);
    h.get_by_label("Compare Files");
    h.get_by_label("Compare").click();
    h.run_steps(1);
    wait_for_compare(&mut h);
    assert_eq!(h.state().right, Some(RightPanel::Compare));
    assert_eq!(h.state().toast.clone().unwrap().0, "2 differences found");
    h.get_by_label("2 Replaced");
    h.get_by_label("\"five\" → \"three\"").click();
    h.run_steps(2);
    assert_eq!(h.state().compare.as_ref().unwrap().selected, Some(0));
    let id = h.state().active_ids().unwrap().1;
    let i = h.state().active_ids().unwrap().0;
    assert_eq!(h.state().views[i].compare_marks.len(), 2);
    h.get_by_label("Mark as comments").click();
    h.run_steps(3);
    // One page edited in place: two highlights, no inserted/deleted page notes.
    let notes: Vec<_> = h.state().session.get(id).unwrap().info.annotations.iter().filter_map(|a| a.contents.clone()).collect();
    assert_eq!(notes.len(), 2, "{notes:?}");
    assert!(notes.iter().all(|n| n.starts_with("Replaced: ")), "{notes:?}");
    h.get_by_label("Report…").click();
    h.run_steps(3);
    assert_eq!(h.state().session.get(h.state().active_ids().unwrap().1).unwrap().name, "Compare Report.pdf");
}

/// Let the background comparison finish (bounded), then draw its result.
fn wait_for_compare(h: &mut Harness<'static, PdfCraftApp>) {
    for _ in 0..600 {
        if h.state().compare_run.is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        h.run_steps(1);
    }
    assert!(h.state().compare_run.is_none(), "the comparison did not finish");
    h.run_steps(3);
}

const ALPHA: &str = "Alpha page about the lease terms, the monthly rent, the deposit and the dates it is due.";
const BRAVO: &str = "Bravo page about parking spaces, bicycle storage, the shared laundry room and its hours.";
const CHARLIE: &str = "Charlie page about pets, noise after ten at night, smoking and guests staying over weekends.";
const DELTA: &str = "Delta page with the signatures of both parties and the date the agreement was signed.";
const ECHO: &str = "Echo page about the new garden behind the building and who may use it in summer.";

/// Four pages; the new version moves Charlie to the front (one word changed), drops Bravo,
/// inserts Echo and pushes Delta's text down its page.
fn page_versions() -> (Vec<u8>, Vec<u8>) {
    let s = Session::new();
    let v1 = s.create_from_text("t", &[ALPHA, BRAVO, CHARLIE, DELTA].join("\u{c}")).unwrap().to_vec();
    let c2 = CHARLIE.replace("pets", "cats");
    let d2 = format!("\n\n\n\n{DELTA}");
    let v2 = s.create_from_text("t", &[c2.as_str(), ALPHA, ECHO, d2.as_str()].join("\u{c}")).unwrap().to_vec();
    (v1, v2)
}

fn two_files(v1: Vec<u8>, v2: Vec<u8>) -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("v1.pdf", None, v1.clone()).unwrap();
        app.open_bytes("v2.pdf", None, v2.clone()).unwrap();
        app
    });
    h.run_steps(4);
    h
}

/// The page map in the panel, the visual settings and page ranges in the dialog, and saving
/// the JSON report and the marked-up copy, all from one comparison (each page's text read
/// once, on a worker thread).
#[test]
fn compare_page_map_ranges_and_saved_results() {
    let (v1, v2) = page_versions();
    let mut h = two_files(v1, v2);
    assert!(h.state_mut().execute("doc.compare"));
    h.run_steps(2);
    // The visual settings are in the dialog, on by default.
    h.get_by_label("Also compare how the pages look");
    h.get_by_label("Colour tolerance");
    h.get_by_label("Change threshold");
    h.get_by_role_and_label(Role::Button, "Compare").click();
    h.run_steps(1);
    wait_for_compare(&mut h);
    assert_eq!(h.state().right, Some(RightPanel::Compare));
    // The page map: new-document order, deleted pages where they were.
    h.get_by_label("1 inserted, 1 deleted, 1 moved, 0 modified pages");
    h.get_by_label("Old page 3 → new page 1 · Moved");
    h.get_by_label("Old page 1 → new page 2 · Same");
    h.get_by_label("Old page 2 → new page — · Deleted");
    h.get_by_label("Old page — → new page 3 · Inserted");
    h.get_by_label("Old page 4 → new page 4 · Same");
    // The moved page is diffed against its counterpart: one word.
    h.get_by_label("\"pets,\" → \"cats,\"");
    let (vi, new) = h.state().active_ids().unwrap();
    {
        let c = h.state().compare.as_ref().unwrap();
        // Delta's text moved down: it looks different though its words are the same.
        assert_eq!(c.result.visual.iter().map(|v| (v.old_page, v.new_page)).collect::<Vec<_>>(), [(2, 0), (3, 3)]);
        assert!(c.opts.visual && c.opts.old_pages.is_none());
    }
    assert!(h.state().views[vi].compare_marks.iter().any(|m| m.0 == 3), "the shifted page is shaded");
    // A page map row goes to that page of the new file.
    h.get_by_label("Old page — → new page 3 · Inserted").click();
    h.run_steps(2);
    assert_eq!(h.state().views[vi].current, 2);

    // Save JSON report… and Save marked-up copy… (to the test's path, no picker).
    let dir = std::env::temp_dir().join(format!("pdfcraft-compare-ui-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let json = dir.join("report.json");
    h.state_mut().save_override = Some(json.to_string_lossy().into_owned());
    h.get_by_label("Save JSON report…").click();
    h.run_steps(2);
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
    assert_eq!(v["summary"]["pages_moved"], 1);
    assert_eq!(v["summary"]["pages_deleted"], 1);
    assert_eq!(v["pages"][0]["status"], "moved");
    assert_eq!(v["old"]["name"], "v1.pdf");
    assert_eq!(v["new"]["name"], "v2.pdf");
    assert_eq!(v["visual"].as_array().unwrap().len(), 2);
    let overlay = dir.join("marked.pdf");
    h.state_mut().save_override = Some(overlay.to_string_lossy().into_owned());
    h.get_by_label("Save marked-up copy…").click();
    h.run_steps(2);
    let mut s = Session::new();
    let oid = s.open("marked.pdf", None, std::sync::Arc::new(std::fs::read(&overlay).unwrap()), None).unwrap();
    let marks: Vec<String> =
        s.get(oid).unwrap().info.annotations.iter().filter(|a| a.author.as_deref() == Some("Compare")).filter_map(|a| a.contents.clone()).collect();
    for want in ["Moved page: page 3", "Deleted page: page 2", "Inserted page", "Replaced: \"pets,\"", "Looks different"] {
        assert!(marks.iter().any(|m| m.starts_with(want)), "{want} missing from {marks:?}");
    }
    // The open file is not changed by saving the copy.
    assert!(h.state().session.get(new).unwrap().info.annotations.is_empty());

    // Page ranges: old page 1 against new page 2 (the same Alpha page), and a threshold.
    assert!(h.state_mut().execute("doc.compare"));
    h.run_steps(2);
    for _ in 0..2 {
        // Untick "All pages" for the old file, then the new one.
        h.get_all_by_label("All pages").find(|n| n.accesskit_node().toggled() == Some(egui::accesskit::Toggled::True)).unwrap().click();
        h.run_steps(1);
    }
    {
        let d = &mut h.state_mut().compare_draft;
        assert!(!d.old_all && !d.new_all);
        (d.old_from, d.old_to, d.new_from, d.new_to) = (1, 1, 2, 2);
    }
    h.run_steps(1);
    h.get_by_role_and_label(Role::Button, "Compare").click();
    h.run_steps(1);
    wait_for_compare(&mut h);
    assert_eq!(h.state().toast.clone().unwrap().0, "The text is the same");
    h.get_by_label("Old page 1 → new page 2 · Same");
    assert!(h.query_by_label("Old page 3 → new page 1 · Moved").is_none());
    let c = h.state().compare.as_ref().unwrap();
    assert_eq!((c.opts.old_pages, c.opts.new_pages), (Some((0, 0)), Some((1, 1))));
    assert!(c.result.identical());
    // A threshold above the pages' changed share: no page looks different.
    {
        let d = &mut h.state_mut().compare_draft;
        (d.old_all, d.new_all, d.threshold) = (true, true, 50.0);
    }
    h.state_mut().run_compare();
    wait_for_compare(&mut h);
    let c = h.state().compare.as_ref().unwrap();
    assert!(c.result.visual.is_empty() && (c.opts.threshold - 0.5).abs() < 1e-9);
    h.get_by_label("0 Visual");
}

/// A long visual comparison shows its progress in the panel and stops on request.
#[test]
fn a_long_comparison_can_be_stopped() {
    let pages: Vec<String> = (0..150).map(|i| format!("Page {i}: {ALPHA} {BRAVO}")).collect();
    let s = Session::new();
    let v1 = s.create_from_text("t", &pages.join("\u{c}")).unwrap().to_vec();
    let v2 = v1.clone();
    let mut h = two_files(v1, v2);
    {
        let app = h.state_mut();
        app.compare_old = app.views.first().map(|v| v.id);
        app.compare_draft.dpi = 150;
        app.run_compare();
    }
    h.run_steps(1);
    assert!(h.state().compare_run.is_some(), "still running");
    let (done, total, _) = h.state().compare_run.as_ref().unwrap().snapshot().unwrap();
    assert!(done < total, "{done} of {total}");
    h.get_by_label("Stop").click();
    h.run_steps(1);
    wait_for_compare(&mut h);
    // Only a run stopped before its end gives no result ("Comparison stopped").
    assert_eq!(h.state().toast.clone().unwrap().0, "Comparison stopped");
    assert!(h.state().compare.is_none());
    h.get_by_label("Compare this file with an older version: Compare files… in All tools.");
}

/// Writes a screenshot of the Compare panel when PDFCRAFT_SHOT is set (for review).
#[test]
fn compare_panel_screenshot() {
    let Ok(out) = std::env::var("PDFCRAFT_SHOT") else { return };
    let s = Session::new();
    let v1 = s.create_from_text("t", "Delivery within five days. Returns accepted.").unwrap().to_vec();
    let v2 = s.create_from_text("t", "Delivery within three days. Returns accepted for a week.").unwrap().to_vec();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("v1.pdf", None, v1.clone()).unwrap();
        app.open_bytes("v2.pdf", None, v2.clone()).unwrap();
        app
    });
    h.run_steps(4);
    h.state_mut().compare_old = h.state().views.first().map(|v| v.id);
    h.state_mut().run_compare();
    for _ in 0..20 {
        h.run_steps(2);
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    h.render().unwrap().save(out).unwrap();
}

#[test]
fn pdfa_dialog_verifies_and_converts() {
    let doc = Session::new().create_from_text("t", "Keep forever").unwrap().to_vec();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("keep.pdf", None, doc.clone()).unwrap();
        app
    });
    h.run_steps(4);
    assert!(h.state_mut().execute("standards.pdfa"));
    h.run_steps(2);
    h.get_by_label("Declared conformance: none");
    h.get_by_label("Verify").click();
    h.run_steps(3);
    h.get_by_label("The document has no XMP metadata");
    h.get_by_label("Save as PDF/A").click();
    h.run_steps(3);
    h.get_by_label("Declared conformance: PDF/A-2b");
    let issues = h.state().pdfa.issues.clone().unwrap();
    assert!(issues.iter().all(|i| !i.fixable), "{issues:?}");
    let id = h.state().active_ids().unwrap().1;
    assert_eq!(h.state().session.get(id).unwrap().can_undo(), Some("Save as PDF/A-2b"));
}

#[test]
fn export_to_word_html_and_rtf() {
    let doc = Session::new().create_from_text("t", "Exported words").unwrap().to_vec();
    let dir = std::env::temp_dir().join(format!("pdfcraft-office-ui-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut app = PdfCraftApp::new();
    app.set_option("language", "en").unwrap();
    app.open_bytes("e.pdf", None, doc).unwrap();
    for ext in ["docx", "html", "rtf"] {
        let out = dir.join(format!("e.{ext}"));
        app.save_override = Some(out.to_string_lossy().into_owned());
        assert!(app.execute(&format!("export.{ext}")));
        assert!(std::fs::metadata(&out).unwrap().len() > 40, "{ext}");
    }
    assert!(std::fs::read_to_string(dir.join("e.html")).unwrap().contains("Exported words"));
}

#[test]
fn export_to_excel_writes_a_workbook() {
    let doc = Session::new().create_from_text("t", "Exported cells").unwrap().to_vec();
    let dir = std::env::temp_dir().join(format!("pdfcraft-xlsx-ui-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut app = PdfCraftApp::new();
    app.set_option("language", "en").unwrap();
    app.open_bytes("e.pdf", None, doc).unwrap();
    let out = dir.join("e.xlsx");
    app.save_override = Some(out.to_string_lossy().into_owned());
    assert!(app.execute("export.xlsx"));
    let bytes = std::fs::read(&out).unwrap();
    assert!(bytes.starts_with(b"PK\x03\x04"));
    assert!(bytes.windows(b"xl/worksheets/sheet1.xml".len()).any(|w| w == b"xl/worksheets/sheet1.xml"));
}
