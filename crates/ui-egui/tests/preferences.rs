//! The Preferences window: categories, search, settings that take effect where the app reads
//! them (autosave, recent files, accessibility, signing defaults), `ui.set pref.<key>`, and
//! settings files that are old, corrupt or hostile.

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::PdfCraftApp;
use pdfcraft_ui_egui::prefs_ui::Page;
use serde_json::json;

const PDF: &[u8] = b"%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] >> endobj
trailer << /Root 1 0 R >>
%%EOF";

fn harness() -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(|_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.set_option("dialog", "preferences").unwrap();
        app
    });
    h.run_steps(3);
    h
}

#[test]
fn categories_search_and_accessibility_settings() {
    let mut h = harness();
    // Every category is listed; General is shown first.
    for page in ["General", "Documents", "Commenting", "Forms", "Identity", "Signatures", "Security", "Accessibility", "Color management"] {
        h.get_by_role_and_label(Role::Button, page);
    }
    h.get_by_label("Interface language");
    assert!(h.query_by_label("Reduce motion").is_none());

    // Accessibility: reduce motion, high contrast and text size take effect at once.
    h.get_by_role_and_label(Role::Button, "Accessibility").click();
    h.run_steps(2);
    assert_eq!(h.state().prefs_ui.page, Page::Accessibility);
    h.get_by_role_and_label(Role::CheckBox, "Reduce motion").click();
    h.run_steps(2);
    assert!(h.state().session.prefs().reduce_motion);
    assert_eq!(h.ctx.global_style().animation_time, 0.0);
    h.get_by_role_and_label(Role::CheckBox, "High contrast").click();
    h.run_steps(2);
    let t = pdfcraft_ui_egui::theme::Tokens::get(&h.ctx);
    assert_eq!(t.text_muted, t.text, "secondary text is as strong as body text");
    // The theme changing keeps the overlay.
    h.state_mut().set_option("theme", "dark").unwrap();
    h.run_steps(2);
    let t = pdfcraft_ui_egui::theme::Tokens::get(&h.ctx);
    assert!(t.dark() && t.text_muted == t.text);
    assert_eq!(h.ctx.global_style().animation_time, 0.0);
    h.state_mut().set_option("pref.accessibility.text_scale", "1.25").unwrap();
    h.run_steps(2);
    assert_eq!(h.ctx.zoom_factor(), 1.25);
    h.state_mut().set_option("pref.accessibility.text_scale", "1").unwrap();
    h.state_mut().set_option("pref.accessibility.reduce_motion", "off").unwrap();
    h.run_steps(2);
    assert_eq!(h.ctx.zoom_factor(), 1.0);
    assert!(h.ctx.global_style().animation_time > 0.0);

    // Search filters every page; a match is changed right there.
    h.state_mut().prefs_ui.query = "autosave".into();
    h.run_steps(2);
    h.get_by_label("Save recovery copies of unsaved changes every");
    assert!(h.query_by_label("High contrast").is_none());
    assert!(h.query_by_label("Interface language").is_none());
    h.state_mut().prefs_ui.query = "colour".into();
    h.run_steps(2);
    h.get_by_label("Field highlight color");
    h.get_by_label("Rendering intent");
    h.state_mut().prefs_ui.query = "qqqq".into();
    h.run_steps(2);
    h.get_by_label_contains("No preferences match");
    // Choosing a category clears the search.
    h.get_by_role_and_label(Role::Button, "Security").click();
    h.run_steps(2);
    assert!(h.state().prefs_ui.query.is_empty());
    h.get_by_role_and_label(Role::CheckBox, "Enable Acrobat JavaScript").click();
    h.run_steps(2);
    assert!(!h.state().session.javascript());
    h.get_by_label("OK").click();
    h.run_steps(2);
    assert_eq!(h.state().dialog, None);
}

#[test]
fn documents_identity_and_signing_preferences_take_effect() {
    let dir = std::env::temp_dir().join(format!("pdfcraft-prefs-ui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut app = PdfCraftApp::new();
    app.set_option("language", "en").unwrap();

    // Autosave interval: minutes, or off.
    assert_eq!(app.autosave_interval(), Some(60.0));
    app.set_option("pref.documents.autosave_minutes", "5").unwrap();
    assert_eq!(app.autosave_interval(), Some(300.0));
    app.set_option("pref.documents.autosave_minutes", "0").unwrap();
    assert_eq!(app.autosave_interval(), None);

    // Recent files: lowering the count trims the list, and opening keeps it at the count.
    for i in 0..5 {
        let path = dir.join(format!("r{i}.pdf"));
        std::fs::write(&path, PDF).unwrap();
        app.recent.push(pdfcraft_ui_egui::RecentFile { name: format!("r{i}.pdf"), path: path.to_string_lossy().into_owned(), pages: 1, size: 0 });
    }
    app.set_option("pref.documents.recent_files", "3").unwrap();
    assert_eq!(app.recent.len(), 3);
    let saved = app.persist();
    let mut fresh = PdfCraftApp::new();
    fresh.restore(&saved);
    assert_eq!((fresh.recent.len(), fresh.autosave_interval()), (3, None));

    // Identity and signing defaults seed a new signature and a new digital ID.
    app.open_bytes("doc.pdf", None, PDF.to_vec()).unwrap();
    app.set_option("pref.identity.name", "Grace Hopper").unwrap();
    app.set_option("pref.identity.organization", "Navy").unwrap();
    app.set_option("pref.identity.email", "grace@example.com").unwrap();
    app.set_option("pref.signatures.reason", "I approve").unwrap();
    app.set_option("pref.signatures.location", "Arlington").unwrap();
    assert_eq!(app.comment_prefs.author, "Grace Hopper");
    app.start_signing(0, None, None, None);
    let d = app.sign_draft.as_ref().expect("signing starts");
    assert_eq!((d.reason.as_str(), d.location.as_str()), ("I approve", "Arlington"));
    assert_eq!((d.new_id.name.as_str(), d.new_id.organization.as_str(), d.new_id.email.as_str()), ("Grace Hopper", "Navy", "grace@example.com"));

    // App keys and session keys round-trip through the settings file.
    app.set_option("pref.commenting.keep_tool_selected", "on").unwrap();
    app.set_option("pref.forms.highlight_color", "[0, 128, 255]").unwrap();
    app.set_option("pref.color.untagged", "working_space").unwrap();
    app.set_option("pref.general.default_mode", "read").unwrap();
    let saved = app.persist();
    let mut fresh = PdfCraftApp::new();
    fresh.restore(&saved);
    let (a, b) = (app.prefs_values(), fresh.prefs_values());
    for key in [
        "commenting.keep_tool_selected",
        "forms.highlight_color",
        "color.untagged",
        "general.default_mode",
        "identity.name",
        "identity.email",
        "signatures.reason",
    ] {
        assert_eq!(a[key], b[key], "{key}");
    }
    assert_eq!(b["forms.highlight_color"], "#0080FF");
    assert_eq!(fresh.session.prefs().field_highlight, [0, 128, 255]);
    // Clearing the name goes back to the login name for comments.
    app.set_option("pref.identity.name", "   ").unwrap();
    assert!(!app.comment_prefs.author.trim().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hostile_values_and_corrupt_settings_keep_defaults() {
    let mut app = PdfCraftApp::new();
    app.set_option("language", "en").unwrap();
    let defaults = app.prefs_values();
    for (key, value) in [
        ("pref.no.such", "1"),
        ("pref.general.theme", "neon"),
        ("pref.general.language", "xx-YY"),
        ("pref.general.default_mode", ""),
        ("pref.documents.default_zoom", "9000%"),
        ("pref.documents.autosave_minutes", "NaN"),
        ("pref.forms.highlight_color", "[1, 2"),
        ("pref.forms.highlight_color", "#12"),
        ("pref.accessibility.high_contrast", "maybe"),
        ("pref.color.cmyk", "user:nothing"),
        ("pref.security.unlisted_sites", "explode"),
        ("preferences-page", "updates-and-more"),
    ] {
        assert!(app.set_option(key, value).is_err(), "{key}={value}");
    }
    assert_eq!(app.prefs_values(), defaults, "refused values change nothing");
    // Clamped, never trusted.
    app.set_option("pref.documents.recent_files", "1e300").unwrap();
    assert_eq!(app.session.prefs().recent_files, 50);
    app.set_option("pref.accessibility.text_scale", "-7").unwrap();
    assert_eq!(app.session.prefs().text_scale, 0.75);
    app.set_option("pref.forms.required_color", "[999, -1, 0.4]").unwrap();
    assert_eq!(app.pref("forms.required_color"), Some(json!("#FF0000")));
    app.set_option("pref.signatures.reason", &"\u{0}\u{1b}x".repeat(2000)).unwrap();
    let reason = &app.session.prefs().signing_reason;
    assert!(reason.chars().count() <= 200 && !reason.chars().any(char::is_control));
    app.set_option("preferences-page", "COLOR").unwrap();
    assert_eq!(app.prefs_ui.page, Page::Color);

    // Old, corrupt or hostile settings files: defaults, never a crash.
    for file in [
        "",
        "not json",
        "[]",
        "null",
        r#"{"prefs": "garbage"}"#,
        r#"{"prefs": [1, 2, 3]}"#,
        r#"{"prefs": {"documents.autosave_minutes": "soon", "forms.highlight_color": [1, 2], "color.rgb": 7, "accessibility.reduce_motion": {}}}"#,
        r#"{"recent": "x", "comment_tool_pinned": "yes", "prefs": null}"#,
    ] {
        let mut fresh = PdfCraftApp::new();
        fresh.set_option("language", "en").unwrap();
        let before = fresh.prefs_values();
        fresh.restore(file);
        let after = fresh.prefs_values();
        for (k, v) in &before {
            // The identity name follows the login name either way.
            if k != "identity.name" {
                assert_eq!(after.get(k), Some(v), "{file}: {k}");
            }
        }
    }
    let mut fresh = PdfCraftApp::new();
    fresh.restore(r#"{"prefs": {"documents.recent_files": 1e99, "accessibility.text_scale": 1e99, "documents.autosave_minutes": -3}}"#);
    assert_eq!((fresh.session.prefs().recent_files, fresh.session.prefs().text_scale, fresh.autosave_interval()), (50, 2.0, None));
}

#[test]
fn keeping_structure_tags_is_a_visible_setting() {
    let mut h = harness();
    h.get_by_role_and_label(Role::Button, "Documents").click();
    h.run_steps(3);
    assert!(h.state().session.keep_tags_on_edit(), "on by default");
    h.get_by_label("Keep structure tags when editing content").click();
    h.run_steps(3);
    assert!(!h.state().session.keep_tags_on_edit(), "the session follows the box");
    h.get_by_label("Keep structure tags when editing content").click();
    h.run_steps(3);
    assert!(h.state().session.keep_tags_on_edit());
    // Search finds it by what it is about; ui.set pref.<key> reaches it; nonsense is refused.
    h.state_mut().prefs_ui.query = "reading order".into();
    h.run_steps(2);
    h.get_by_label("Keep structure tags when editing content");
    h.get_by_label_contains("structure in the same undo step");
    h.state_mut().set_option("pref.documents.keep_tags_on_edit", "off").unwrap();
    h.run_steps(2);
    assert!(!h.state().session.keep_tags_on_edit());
    assert!(h.state_mut().set_option("pref.documents.keep_tags_on_edit", "sometimes").is_err());
    assert!(!h.state().session.keep_tags_on_edit());
}
