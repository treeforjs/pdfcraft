//! Theme preferences stay in sync across both menus, OS changes and saved settings.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::PdfCraftApp;
use pdfcraft_ui_egui::theme::{ThemeKind, ThemePreference, Tokens};

fn harness() -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1100.0, 800.0)).build_eframe(|_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app
    });
    h.input_mut().system_theme = Some(egui::Theme::Dark);
    h.run_steps(4);
    h
}

fn assert_theme(h: &Harness<'static, PdfCraftApp>, preference: ThemePreference, kind: ThemeKind) {
    assert_eq!(h.state().theme_preference, preference);
    assert_eq!(h.state().theme, kind);
    assert_eq!(Tokens::get(&h.ctx).kind, kind);
    // Cream is a light theme, Dim a dark one.
    let dark = matches!(kind, ThemeKind::Dim | ThemeKind::Dark);
    assert_eq!(kind.is_dark(), dark);
    assert_eq!(h.ctx.global_style().visuals.dark_mode, dark);
    assert_eq!(h.ctx.theme(), if dark { egui::Theme::Dark } else { egui::Theme::Light });
}

fn open_toolbar(h: &mut Harness<'static, PdfCraftApp>, label: &str) {
    h.get_by_label(&format!("Display theme: {label}")).click();
    h.run_steps(3);
    for choice in ["Use system setting", "Light gray", "Cream", "Dim", "Dark gray"] {
        h.get_by_label(choice);
    }
}

fn open_view_menu(h: &mut Harness<'static, PdfCraftApp>) {
    h.get_by_label("Menu").click();
    h.run_steps(2);
    h.get_by_label("View ⏵").hover();
    h.run_steps(3);
    assert!(h.query_by_label("Switch light / dark theme").is_none());
    h.get_by_label("Display theme ⏵").hover();
    h.run_steps(3);
    for choice in ["Use system setting", "Light gray", "Cream", "Dim", "Dark gray"] {
        h.get_by_label(choice);
    }
}

#[test]
fn toolbar_and_view_menu_share_three_state_preferences() {
    let mut h = harness();
    // A fresh install follows the desktop, which is dark here.
    assert_theme(&h, ThemePreference::System, ThemeKind::Dark);
    open_toolbar(&mut h, "Use system setting");
    h.get_by_label("Light gray").click();
    h.run_steps(3);
    // Explicit light wins even on a dark desktop, including egui's popup style.
    assert_theme(&h, ThemePreference::Light, ThemeKind::Light);
    open_toolbar(&mut h, "Light gray");
    h.get_by_label("Use system setting").click();
    h.run_steps(3);
    assert_theme(&h, ThemePreference::System, ThemeKind::Dark);

    open_view_menu(&mut h);
    h.get_by_label("Light gray").click();
    h.run_steps(3);
    assert_theme(&h, ThemePreference::Light, ThemeKind::Light);
    open_toolbar(&mut h, "Light gray");
    h.get_by_label("Dark gray").click();
    h.run_steps(3);
    assert_theme(&h, ThemePreference::Dark, ThemeKind::Dark);

    open_view_menu(&mut h);
    h.get_by_label("Use system setting").click();
    h.run_steps(3);
    assert_theme(&h, ThemePreference::System, ThemeKind::Dark);
    h.input_mut().system_theme = Some(egui::Theme::Light);
    h.run_steps(3);
    assert_theme(&h, ThemePreference::System, ThemeKind::Light);
    open_toolbar(&mut h, "Use system setting");
    h.get_by_label("Dark gray").click();
    h.run_steps(3);
    // A manual choice exits system mode and survives subsequent OS changes.
    h.input_mut().system_theme = Some(egui::Theme::Dark);
    h.run_steps(2);
    h.input_mut().system_theme = Some(egui::Theme::Light);
    h.run_steps(2);
    assert_theme(&h, ThemePreference::Dark, ThemeKind::Dark);
}

#[test]
fn cream_and_dim_are_offered_in_the_toolbar_and_view_menu() {
    let mut h = harness();
    open_toolbar(&mut h, "Use system setting");
    h.get_by_label("Cream").click();
    h.run_steps(3);
    // Cream stays light on a dark desktop.
    assert_theme(&h, ThemePreference::Cream, ThemeKind::Cream);
    open_view_menu(&mut h);
    h.get_by_label("Dim").click();
    h.run_steps(3);
    assert_theme(&h, ThemePreference::Dim, ThemeKind::Dim);
    h.input_mut().system_theme = Some(egui::Theme::Light);
    h.run_steps(3);
    assert_theme(&h, ThemePreference::Dim, ThemeKind::Dim);
    open_toolbar(&mut h, "Dim");
    h.get_by_label("Cream").click();
    h.run_steps(3);
    assert_theme(&h, ThemePreference::Cream, ThemeKind::Cream);
}

#[test]
fn saved_preferences_restore_with_legacy_light_dark_settings() {
    for (value, preference) in [
        ("light", ThemePreference::Light),
        ("cream", ThemePreference::Cream),
        ("dim", ThemePreference::Dim),
        ("dark", ThemePreference::Dark),
        ("system", ThemePreference::System),
        // Older craft theme names map to the nearest choice.
        ("sepia", ThemePreference::Cream),
        ("grey", ThemePreference::Dim),
    ] {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.set_option("theme", value).unwrap();
        let saved = app.persist();
        let mut restored = PdfCraftApp::new();
        restored.set_option("language", "en").unwrap();
        restored.restore(&saved);
        assert_eq!(restored.theme_preference, preference);
    }
    let mut app = PdfCraftApp::new();
    app.set_option("language", "en").unwrap();
    app.restore(r#"{"theme":"Dark"}"#);
    assert_eq!(app.theme_preference, ThemePreference::Dark);
    assert_eq!(app.theme, ThemeKind::Dark);
    app.restore(r#"{"theme":"Light"}"#);
    assert_eq!(app.theme_preference, ThemePreference::Light);
    assert_eq!(app.theme, ThemeKind::Light);
    app.restore(r#"{"theme":"Cream"}"#);
    assert_eq!(app.theme_preference, ThemePreference::Cream);
    assert_eq!(app.theme, ThemeKind::Cream);
    app.restore(r#"{"theme":"Medium"}"#);
    assert_eq!(app.theme_preference, ThemePreference::Dim);
    assert_eq!(app.theme, ThemeKind::Dim);
    app.restore(r#"{"theme":"System"}"#);
    app.restore(r#"{"theme":"Purple"}"#);
    assert_eq!(app.theme_preference, ThemePreference::System);
}

#[test]
fn commands_and_options_apply_before_the_first_frame() {
    let mut app = PdfCraftApp::new();
    app.set_option("language", "en").unwrap();
    app.run_command("view.theme.dark");
    assert_eq!(app.theme_preference, ThemePreference::Dark);
    assert_eq!(app.theme, ThemeKind::Dark);
    app.run_command("view.theme.system");
    assert_eq!(app.theme_preference, ThemePreference::System);
    // The existing palette/control toggle also goes through the preference setter.
    app.run_command("view.theme");
    assert_eq!(app.theme_preference, ThemePreference::Light);
    assert_eq!(app.theme, ThemeKind::Light);
    // The toggle treats Cream as light and Dim as dark.
    app.run_command("view.theme.cream");
    assert_eq!(app.theme, ThemeKind::Cream);
    app.run_command("view.theme");
    assert_eq!(app.theme_preference, ThemePreference::Dark);
    app.run_command("view.theme.dim");
    assert_eq!(app.theme, ThemeKind::Dim);
    app.run_command("view.theme");
    assert_eq!(app.theme_preference, ThemePreference::Light);
    app.run_command("view.theme.light");
    app.set_option("theme", "dark").unwrap();
    assert!(app.set_option("theme", "purple").unwrap_err().contains("system, light, cream, dim or dark"));
    assert_eq!(app.theme_preference, ThemePreference::Dark);
    assert_eq!(app.theme, ThemeKind::Dark);
}
