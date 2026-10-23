//! Print production in the app (egui_kittest): the Output Preview panel (separations with inks
//! switched off, total area coverage highlighted, the ink under the pointer, colour warnings, the
//! Object Inspector), the Ink Manager, Convert Colors and Add Printer Marks dialogs, the Color
//! Management preferences page, and hostile documents (hundreds of spot inks, bad settings).

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::output_preview_ui::PreviewMode;
use pdfcraft_ui_egui::{Dialog, PdfCraftApp, RightPanel};
use serde_json::Value;

/// A 300 × 300 pt page from `content`, with `spots` Separation inks /S0 … as /CS0 … (alternate
/// DeviceCMYK, Type 2 tint transform) and an ExtGState /GS0 that turns fill overprint on.
fn doc(content: &str, spots: usize) -> Vec<u8> {
    let spaces: String = (0..spots)
        .map(|i| format!("/CS{i} [/Separation /S{i} /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0.3 0.9 0] /N 1 >>] "))
        .collect();
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 300 300] >>".to_string(),
        format!(
            "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /ColorSpace << {spaces}>> /ExtGState << /GS0 << /op true /OPM 1 >> >> >> >>"
        ),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
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

/// Rich black in the lower left, a spot ink overprinting in the upper right.
const PAGE: &str = "1 1 1 1 k 20 20 100 100 re f /GS0 gs /CS0 cs 1 scn 180 180 80 80 re f";

fn harness(bytes: Vec<u8>) -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.run_inline = true;
        app.open_bytes("press.pdf", None, bytes).unwrap();
        app
    });
    h.run_steps(4);
    h
}

fn state(h: &Harness<'static, PdfCraftApp>) -> Value {
    h.state().output_preview_state()
}

/// The screen point of user-space point (x, y) on the 300 × 300 pt page.
fn on_page(h: &Harness<'static, PdfCraftApp>, x: f32, y: f32) -> egui::Pos2 {
    let r = h.state().views[0].page_screen_rect(0).unwrap();
    let s = r.width() / 300.0;
    egui::pos2(r.left() + x * s, r.bottom() - y * s)
}

#[test]
fn the_panel_separates_hides_inks_reads_the_pointer_and_highlights_coverage() {
    let mut h = harness(doc(PAGE, 1));
    assert!(h.state_mut().execute("prepress.output_preview"));
    h.run_steps(4);
    assert_eq!(h.state().right, Some(RightPanel::OutputPreview));
    assert!(h.query_all_by_label("Output preview").count() >= 2, "the panel's header beside its rail button");
    let s = state(&h);
    let inks: Vec<&str> = s["preview"]["inks"].as_array().unwrap().iter().map(|i| i["name"].as_str().unwrap()).collect();
    assert_eq!(inks, ["Cyan", "Magenta", "Yellow", "Black", "S0"], "{s}");
    assert_eq!(s["preview"]["coverage_max"], 400.0);
    assert_eq!(s["preview"]["textured"], true);

    // The ink under the pointer: the rich black box puts 100% of each process ink down.
    h.hover_at(on_page(&h, 70.0, 70.0));
    h.run_steps(3);
    let s = state(&h);
    assert_eq!(s["preview"]["pointer"]["total"], 400.0, "{s}");

    // Switching an ink off renders again without it.
    h.get_by_label("S0").click();
    h.run_steps(4);
    let s = state(&h);
    assert_eq!(s["settings"]["hidden_inks"][0], "S0");
    let s0 = s["preview"]["inks"].as_array().unwrap().iter().find(|i| i["name"] == "S0").unwrap().clone();
    assert_eq!(s0["shown"], false, "{s}");
    h.get_by_label("Show all inks").click();
    h.run_steps(4);
    assert!(state(&h)["settings"]["hidden_inks"].as_array().unwrap().is_empty());

    // Total area coverage over 300% is painted in the warning colour.
    h.get_by_label("Highlight coverage over").click();
    h.run_steps(4);
    let s = state(&h);
    assert_eq!(s["settings"]["coverage_warning"], true);
    assert_eq!(s["preview"]["over_limit"], true);
    assert!(s["preview"]["warned_pixels"].as_u64().unwrap() > 0, "{s}");
    h.state_mut().set_option("output-preview-coverage", "off").unwrap();
    h.run_steps(4);
    assert_eq!(state(&h)["preview"]["warned_pixels"], 0);

    // The simulation follows its check boxes.
    h.get_by_label("Simulate paper color").click();
    h.run_steps(4);
    let s = state(&h);
    assert_eq!(s["settings"]["simulate_paper"], true);
    assert_eq!(s["settings"]["simulate_black"], true);
    h.state_mut().set_option("output-preview-show", "images").unwrap();
    h.run_steps(4);
    assert_eq!(state(&h)["preview"]["coverage_max"], 0.0, "only images shown: nothing inks the page");

    // Nothing is cached or computed once the panel closes.
    h.state_mut().set_option("output-preview", "closed").unwrap();
    h.run_steps(3);
    assert_eq!(h.state().right, None);
    let s = state(&h);
    assert!(s["preview"].is_null() && !s["busy"].as_bool().unwrap(), "{s}");
    // The document itself never changed.
    let doc = h.state().session.get(h.state().views[0].id).unwrap();
    assert!(!doc.dirty && doc.can_undo().is_none());
}

#[test]
fn colour_warnings_and_the_object_inspector_describe_the_page() {
    let mut h = harness(doc(PAGE, 1));
    assert!(h.state_mut().execute("prepress.color_warnings"));
    h.run_steps(4);
    let s = state(&h);
    assert_eq!(s["mode"], "warnings");
    let kinds: Vec<&str> = s["warnings"]["kinds"].as_array().unwrap().iter().map(|k| k.as_str().unwrap()).collect();
    assert!(kinds.contains(&"rich-black"), "{s}");
    assert!(kinds.iter().any(|k| k.starts_with("overprint")), "{s}");
    h.get_by_label("Rich black");

    // A click in Inspector mode lists what is under it.
    assert!(h.state_mut().execute("prepress.object_inspector"));
    h.run_steps(2);
    h.get_by_label("Click on the page to list the objects under the pointer.");
    let at = on_page(&h, 220.0, 220.0);
    h.hover_at(at);
    h.run_steps(2);
    h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    h.run_steps(4);
    let s = state(&h);
    let objects = s["inspection"]["objects"].as_array().unwrap();
    assert_eq!(objects.len(), 1, "{s}");
    assert_eq!(objects[0]["kind"], "path");
    assert_eq!(objects[0]["fill"], "Separation (S0)");
    h.get_by_label("Path");
    // The same through ui.set, at a point with nothing painted.
    h.state_mut().set_option("output-preview-inspect", "150,150").unwrap();
    h.run_steps(4);
    assert!(state(&h)["inspection"]["objects"].as_array().unwrap().is_empty());
    h.get_by_label("Nothing is painted here.");
}

#[test]
fn the_ink_manager_converts_and_aliases_for_the_session_and_applies_as_one_undo_step() {
    let mut h = harness(doc(PAGE, 2));
    assert!(h.state_mut().execute("prepress.ink_manager"));
    h.run_steps(3);
    assert_eq!(h.state().dialog, Some(Dialog::InkManager));
    let s = h.state().ink_manager_state();
    let names: Vec<&str> = s["inks"].as_array().unwrap().iter().map(|i| i["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Cyan", "Magenta", "Yellow", "Black", "S0", "S1"], "{s}");
    h.get_by_label("All spots to process").click();
    h.run_steps(2);
    h.get_by_label("OK").click();
    h.run_steps(3);
    assert_eq!(h.state().dialog, None);
    let id = h.state().views[0].id;
    assert!(h.state().session.get(id).unwrap().ink_manager().all_to_process);
    // Output Preview follows the session settings: the spot inks print as process.
    h.state_mut().set_option("output-preview", "separations").unwrap();
    h.run_steps(4);
    let s = state(&h);
    assert!(s["preview"]["inks"].as_array().unwrap().iter().all(|i| i["name"] != "S0"), "{s}");

    // Apply to document: one undo step.
    assert!(h.state_mut().execute("prepress.ink_manager"));
    h.run_steps(3);
    h.get_by_label("Apply to document").click();
    h.run_steps(3);
    let doc = h.state().session.get(id).unwrap();
    assert_eq!(doc.can_undo(), Some("Apply Ink Manager"));
    assert!(doc.dirty);

    // A cycle is refused with a reason and changes nothing.
    assert!(h.state_mut().execute("prepress.ink_manager"));
    h.run_steps(3);
    let d = &mut h.state_mut().ink_manager;
    d.settings = Default::default();
    d.settings.aliases.insert("S0".into(), "S1".into());
    d.settings.aliases.insert("S1".into(), "S0".into());
    h.run_steps(1);
    h.get_by_label("OK").click();
    h.run_steps(2);
    assert_eq!(h.state().dialog, Some(Dialog::InkManager));
    assert!(h.state().ink_manager.error.is_some());
}

#[test]
fn convert_colors_and_printer_marks_change_the_document_once() {
    let mut h = harness(doc(PAGE, 1));
    let id = h.state().views[0].id;
    assert!(h.state_mut().execute("prepress.convert_colors"));
    h.run_steps(3);
    assert_eq!(h.state().dialog, Some(Dialog::ConvertColors));
    h.state_mut().convert_draft.destination = "DeviceGray";
    h.run_steps(1);
    // The header's Convert mode shares the label; the dialog's button is drawn last.
    h.query_all_by_label("Convert").last().unwrap().click();
    h.run_steps(3);
    assert_eq!(h.state().dialog, None);
    assert_eq!(h.state().session.get(id).unwrap().can_undo(), Some("Convert colors"));

    assert!(h.state_mut().execute("prepress.marks"));
    h.run_steps(3);
    assert_eq!(h.state().dialog, Some(Dialog::PrinterMarks));
    h.get_by_label("Add marks").click();
    h.run_steps(3);
    assert_eq!(h.state().dialog, None);
    let doc = h.state().session.get(id).unwrap();
    assert_eq!(doc.can_undo(), Some("Add printer marks"));
    assert!(doc.info.pages[0].width > 300.0, "the media box grows round the trim: {}", doc.info.pages[0].width);

    // No mark chosen: refused, nothing changes.
    assert!(h.state_mut().execute("prepress.marks"));
    h.run_steps(2);
    let s = &mut h.state_mut().printer_marks_draft.settings;
    (s.trim, s.bleed, s.registration, s.color_bars, s.page_info) = (false, false, false, false, false);
    h.run_steps(1);
    h.get_by_label("Add marks").click();
    h.run_steps(2);
    assert_eq!(h.state().dialog, Some(Dialog::PrinterMarks));
    assert_eq!(h.state().session.get(id).unwrap().can_undo(), Some("Add printer marks"));
}

#[test]
fn the_color_management_page_loads_profiles_and_restores_defaults() {
    let mut h = harness(doc(PAGE, 0));
    assert!(h.state_mut().execute("app.color_management"));
    h.run_steps(3);
    assert_eq!(h.state().dialog, Some(Dialog::Preferences));
    h.get_by_label("RGB working space");
    let (_, bytes) = h.state().session.color_management().profile("srgb").unwrap();
    let path = std::env::temp_dir().join(format!("press-proof-{}.icc", std::process::id()));
    std::fs::write(&path, bytes.as_slice()).unwrap();
    h.state_mut().load_color_profile_file(&path);
    h.run_steps(3);
    let id = format!("user:press-proof-{}", std::process::id());
    assert!(h.state().session.color_management().profile(&id).is_some(), "loaded as {id}");
    // A file that is no profile is refused, not loaded.
    std::fs::write(&path, b"not a profile").unwrap();
    h.state_mut().load_color_profile_file(&path);
    assert_eq!(h.state().session.color_management().profiles().iter().filter(|p| p.id.starts_with("user:")).count(), 1);
    let _ = std::fs::remove_file(&path);
    h.state_mut().set_option("pref.color.rgb", &id).unwrap();
    h.run_steps(2);
    h.get_by_label("Reset").click();
    h.run_steps(2);
    assert_eq!(h.state().session.color_management().settings(), &Default::default());
}

#[test]
fn hundreds_of_spot_inks_list_and_preview_without_stalling() {
    let spots = 300;
    let content: String = (0..spots).map(|i| format!("/CS{i} cs 1 scn {} {} 8 8 re f ", (i % 30) * 9 + 10, (i / 30) * 9 + 10)).collect();
    let mut h = harness(doc(&content, spots));
    assert!(h.state_mut().execute("prepress.output_preview"));
    h.run_steps(4);
    let s = state(&h);
    // The separations list is cut to what the ink rasterizer separates; the panel lays out only
    // the rows on screen.
    let shown = s["preview"]["inks"].as_array().map_or(0, Vec::len);
    let error = s["preview_error"].as_str().unwrap_or_default().to_string();
    assert!(shown > 4 || !error.is_empty(), "{s}");
    let started = std::time::Instant::now();
    h.run_steps(10);
    assert!(started.elapsed() < std::time::Duration::from_secs(20), "ten frames took {:?}", started.elapsed());
    assert!(h.state_mut().execute("prepress.ink_manager"));
    h.run_steps(3);
    // The ink list stops where the ink rasterizer does (64 inks), never at the document's count.
    let s = h.state().ink_manager_state();
    let listed = s["inks"].as_array().unwrap().len();
    assert!(listed > 4 && listed <= spots + 4, "{listed}");
    h.get_by_label("All spots to process").click();
    h.run_steps(2);
    h.get_by_label("OK").click();
    h.run_steps(3);

    // Hostile settings through ui.set are refused with what is allowed.
    let app = h.state_mut();
    assert!(app.set_option("output-preview-coverage", "5000").is_err());
    assert!(app.set_option("output-preview-coverage", "NaN").is_err());
    assert!(app.set_option("output-preview-inspect", "nan,1").is_err());
    assert!(app.set_option("output-preview-inspect", "1").is_err());
    assert!(app.set_option("output-preview", "bogus").is_err());
    assert!(app.set_option("output-preview-show", "everything").is_err());
    assert!(app.set_option("output-preview-ink", &"x".repeat(500)).is_err());
    assert!(app.set_option("output-preview-overprint", "maybe").is_err());
    // A point far off the page inspects nothing and doesn't panic.
    app.set_option("output-preview-inspect", "1e300,-1e300").unwrap();
    h.run_steps(4);
    let s = state(&h);
    assert!(s["inspection"]["objects"].as_array().is_some_and(Vec::is_empty) || s["inspection"]["error"].is_string(), "{s}");
    assert_eq!(h.state().output_preview.mode, PreviewMode::Inspector);
}
