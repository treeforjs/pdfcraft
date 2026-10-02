//! Prepare a form in the real shell (egui_kittest): field tools, placing, selecting, moving,
//! Field Properties and deleting.

use egui::Pos2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use printcraft_ui_egui::{PrintCraftApp, QuickTool};

fn harness() -> Harness<'static, PrintCraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(|_cc| {
        let mut app = PrintCraftApp::new();
        app.open_bytes("form.pdf", None, include_bytes!("data/form.pdf").to_vec()).unwrap();
        app.set_option("zoom", "150").unwrap();
        app
    });
    for _ in 0..60 {
        h.run_steps(2);
        if !h.state().render_pending() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    h
}

/// A point on page 1 at (x, y) points from its top-left.
fn at(h: &Harness<'static, PrintCraftApp>, x: f32, y: f32) -> Pos2 {
    let r = h.state().views[0].page_screen_rect(0).expect("on screen");
    let k = r.width() / 300.0;
    r.min + egui::vec2(x * k, y * k)
}

fn drag(h: &mut Harness<'static, PrintCraftApp>, a: Pos2, b: Pos2) {
    h.hover_at(a);
    h.run_steps(1);
    h.drag_at(a);
    h.run_steps(1);
    for k in 1..=4 {
        h.hover_at(a + (b - a) * (k as f32 / 4.0));
        h.run_steps(1);
    }
    h.drop_at(b);
    h.run_steps(4);
}

fn names(h: &Harness<'static, PrintCraftApp>) -> Vec<String> {
    let s = h.state();
    s.session.get(s.views[0].id).unwrap().form.iter().map(|f| f.name.clone()).collect()
}

fn rect_of(h: &Harness<'static, PrintCraftApp>, name: &str) -> [f64; 4] {
    let s = h.state();
    s.session.get(s.views[0].id).unwrap().form.iter().find(|f| f.name == name).unwrap().widgets[0].rect
}

#[test]
fn placing_moving_editing_and_deleting_a_field() {
    let mut h = harness();
    let before = names(&h).len();
    assert!(h.state_mut().execute("form.add.text"));
    h.run_steps(2);
    h.get_by_label("Prepare a form");
    // Drag out a text field in the empty lower part of the page.
    let (a, b) = (at(&h, 40.0, 300.0), at(&h, 200.0, 322.0));
    drag(&mut h, a, b);
    let all = names(&h);
    assert_eq!(all.len(), before + 1, "{all:?}");
    assert_eq!(all.last().map(String::as_str), Some("Text1"));
    assert_eq!(h.state().quick_tool, QuickTool::Select, "back to Select after one field");
    h.run_steps(2);
    assert_eq!(h.state().views[0].prepare.selected.as_ref().map(|s| s.0.as_str()), Some("Text1"), "the new field is selected");
    let r = rect_of(&h, "Text1");
    assert!((r[2] - r[0] - 160.0).abs() < 2.0 && (r[3] - r[1] - 22.0).abs() < 2.0, "{r:?}");
    // Move it 20 pt right.
    let c = at(&h, 120.0, 311.0);
    let d = c + egui::vec2(at(&h, 20.0, 0.0).x - at(&h, 0.0, 0.0).x, 0.0);
    drag(&mut h, c, d);
    let m = rect_of(&h, "Text1");
    assert!((m[0] - r[0] - 20.0).abs() < 1.5, "moved: {r:?} → {m:?}");
    // Field Properties: rename and make it required (one undo step).
    h.state_mut().execute("form.field.properties");
    h.run_steps(2);
    h.get_by_label("Text Field Properties");
    {
        let d = h.state_mut().field_props.as_mut().expect("open");
        d.name = "email".into();
        d.required = true;
    }
    h.get_by_label("OK").click();
    h.run_steps(3);
    assert!(names(&h).contains(&"email".to_string()));
    {
        let s = h.state();
        let doc = s.session.get(s.views[0].id).unwrap();
        assert_eq!(doc.can_undo(), Some("Change field properties"));
        assert!(doc.form.iter().find(|f| f.name == "email").unwrap().has(printcraft_engine::field_flags::REQUIRED));
        assert_eq!(s.views[0].prepare.selected.as_ref().map(|s| s.0.as_str()), Some("email"));
    }
    // Delete removes the selected field.
    h.key_press(egui::Key::Delete);
    h.run_steps(3);
    assert_eq!(names(&h).len(), before);
    // A click with the check box tool places a default-size one.
    h.state_mut().execute("form.add.checkbox");
    h.run_steps(1);
    let p = at(&h, 40.0, 250.0);
    h.hover_at(p);
    h.run_steps(1);
    h.drag_at(p);
    h.run_steps(1);
    h.drop_at(p);
    h.run_steps(4);
    let r = rect_of(&h, "Check Box1");
    assert_eq!((r[2] - r[0], r[3] - r[1]), (14.0, 14.0));
}
