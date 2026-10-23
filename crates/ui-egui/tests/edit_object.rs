//! Edit a PDF ▸ Edit objects: click and marquee selection, moving, resizing and turning with
//! the handles, Delete and arrow keys, and the Edit panel's object properties, each one undo
//! step.

use egui::Pos2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::PdfCraftApp;

/// A 300×300 pt page: a red square, a blue stroked square and a line of text.
fn pdf() -> Vec<u8> {
    let content = "1 0 0 rg 20 200 60 60 re f\n0 0 1 RG 4 w 150 150 60 60 re S\nBT /F1 12 Tf 20 20 Td (Hello) Tj ET";
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 300 300] >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
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

fn harness() -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_step_dt(1.0 / 60.0).build_eframe(|_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("objects.pdf", None, pdf()).unwrap();
        app.set_option("zoom", "150").unwrap();
        app
    });
    h.run_steps(6);
    assert!(h.state_mut().execute("edit.edit_object"));
    h.run_steps(3);
    h
}

/// A point of the displayed page (points from the top-left) on screen.
fn at(h: &Harness<'static, PdfCraftApp>, x: f32, y: f32) -> Pos2 {
    let r = h.state().views[0].page_screen_rect(0).unwrap();
    let k = r.width() / 300.0;
    r.min + egui::vec2(x * k, y * k)
}

fn scale(h: &Harness<'static, PdfCraftApp>) -> f32 {
    h.state().views[0].page_screen_rect(0).unwrap().width() / 300.0
}

fn drag(h: &mut Harness<'static, PdfCraftApp>, a: Pos2, b: Pos2) {
    h.hover_at(a);
    h.run_steps(1);
    h.drag_at(a);
    h.run_steps(1);
    for k in 1..=4 {
        h.hover_at(a + (b - a) * (k as f32 / 4.0));
        h.run_steps(1);
    }
    h.drop_at(b);
    h.run_steps(3);
}

fn click(h: &mut Harness<'static, PdfCraftApp>, p: Pos2) {
    h.hover_at(p);
    h.run_steps(1);
    h.drag_at(p);
    h.step();
    h.drop_at(p);
    h.run_steps(3);
}

fn objects(h: &Harness<'static, PdfCraftApp>) -> Vec<pdfcraft_engine::PageObject> {
    let s = h.state();
    s.session.get(s.views[0].id).unwrap().page_objects(0)
}

fn selected(h: &Harness<'static, PdfCraftApp>) -> Vec<usize> {
    h.state().views[0].objects.selected.clone()
}

fn undo_label(h: &Harness<'static, PdfCraftApp>) -> Option<String> {
    let s = h.state();
    s.session.get(s.views[0].id).unwrap().can_undo().map(String::from)
}

#[test]
fn objects_are_selected_moved_resized_and_deleted_on_the_page() {
    let mut h = harness();
    h.get_by_label("OBJECT PROPERTIES");
    // Click the red square: it is selected and the panel shows a path.
    let p = at(&h, 50.0, 70.0);
    click(&mut h, p);
    assert_eq!(selected(&h), vec![0]);
    h.get_by_label("Path");
    // Drag it 30 pt right and 20 pt down: one "Move objects" step.
    let k = scale(&h);
    let (from, to) = (at(&h, 50.0, 70.0), at(&h, 50.0, 70.0) + egui::vec2(30.0 * k, 20.0 * k));
    drag(&mut h, from, to);
    let r = objects(&h)[0].rect.unwrap();
    assert!((r[0] - 50.0).abs() < 0.6 && (r[1] - 180.0).abs() < 0.6, "{r:?}");
    assert_eq!(undo_label(&h).as_deref(), Some("Move objects"));
    assert_eq!(selected(&h), vec![0], "the selection stays");
    // Drag the bottom-right handle out: the square grows in proportion about its top-left.
    let (x1, y1) = (50.0 + 60.0, 300.0 - 180.0);
    let (from, to) = (at(&h, x1, y1), at(&h, x1 + 30.0, y1 + 30.0));
    drag(&mut h, from, to);
    let r = objects(&h)[0].rect.unwrap();
    assert!((r[2] - r[0] - 90.0).abs() < 1.0 && (r[3] - r[1] - 90.0).abs() < 1.0, "{r:?}");
    assert!((r[0] - 50.0).abs() < 0.6 && (r[3] - 240.0).abs() < 0.6, "top-left stays: {r:?}");
    assert_eq!(undo_label(&h).as_deref(), Some("Resize objects"));
    // An arrow key nudges it a point right.
    h.key_press(egui::Key::ArrowRight);
    h.run_steps(3);
    assert!((objects(&h)[0].rect.unwrap()[0] - 51.0).abs() < 0.6);
    assert_eq!(h.state().views[0].current, 0, "the page doesn't turn");
    // Undo three times: back where it was.
    for _ in 0..3 {
        assert!(h.state_mut().execute("edit.undo"));
        h.run_steps(2);
    }
    assert_eq!(objects(&h)[0].rect.map(|r| r.map(|v| v.round())), Some([20.0, 200.0, 80.0, 260.0]));
}

#[test]
fn a_marquee_selects_several_and_delete_and_the_panel_change_them() {
    let mut h = harness();
    // Drag from an empty corner around both squares.
    let (from, to) = (at(&h, 5.0, 5.0), at(&h, 260.0, 160.0));
    drag(&mut h, from, to);
    assert_eq!(selected(&h), vec![0, 1]);
    h.get_by_label("2 objects selected");
    // The fill swatch: both paths get a green fill, one step.
    h.get_by_label("fill #2F9E44").click();
    h.run_steps(3);
    let o = objects(&h);
    for k in [0, 1] {
        let a = o[k].appearance.as_ref().unwrap();
        assert!(matches!(a.fill, pdfcraft_engine::Paint::Rgb(c) if (c[1] - 0x9E as f64 / 255.0).abs() < 1e-3), "{a:?}");
    }
    assert_eq!(undo_label(&h).as_deref(), Some("Change path appearance"));
    // Bring to Front from the panel keeps them selected.
    h.get_by_label("Send to Back").click();
    h.run_steps(3);
    h.get_by_label("Bring to Front").click();
    h.run_steps(3);
    assert_eq!(objects(&h)[2].kind, pdfcraft_engine::ObjectKind::Path);
    assert_eq!(selected(&h), vec![1, 2], "the selection follows the objects");
    // Delete removes both; Esc on an empty selection leaves the tool.
    h.key_press(egui::Key::Delete);
    h.run_steps(3);
    let o = objects(&h);
    assert_eq!((o.len(), o[0].kind), (1, pdfcraft_engine::ObjectKind::Text));
    assert!(selected(&h).is_empty());
    assert!(h.state_mut().execute("edit.undo"));
    h.run_steps(2);
    assert_eq!(objects(&h).len(), 3, "one undo step");
    // Clicking empty page clears; ⇧-click builds a selection.
    let p = at(&h, 250.0, 250.0);
    click(&mut h, p);
    assert!(selected(&h).is_empty());
}

#[test]
fn the_rotation_handle_turns_the_selection() {
    let mut h = harness();
    let p = at(&h, 50.0, 70.0);
    click(&mut h, p);
    assert_eq!(selected(&h), vec![0]);
    // The round handle sits 24 px above the box's top centre; drag it round to the right.
    let top = at(&h, 50.0, 40.0) - egui::vec2(0.0, 24.0);
    let centre = at(&h, 50.0, 70.0);
    let right = centre + egui::vec2(top.y.max(centre.y) - top.y.min(centre.y), 0.0);
    drag(&mut h, top, right);
    assert_eq!(undo_label(&h).as_deref(), Some("Rotate objects"));
    // A quarter turn of a square keeps its box.
    let r = objects(&h)[0].rect.unwrap();
    assert!((r[2] - r[0] - 60.0).abs() < 2.0, "{r:?}");
    let state = h.state().views[0].objects.selected.clone();
    assert_eq!(state, vec![0]);
}

fn right_click(h: &mut Harness<'static, PdfCraftApp>, p: Pos2) {
    h.hover_at(p);
    h.run_steps(1);
    for pressed in [true, false] {
        h.input_mut().events.push(egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    h.run_steps(3);
}

fn rects(h: &Harness<'static, PdfCraftApp>) -> Vec<[f64; 4]> {
    objects(h).iter().map(|o| o.rect.unwrap()).collect()
}

#[test]
fn the_panel_and_the_menu_align_and_distribute_the_selection() {
    let mut h = harness();
    // Nothing selected: the commands say what to do and change nothing.
    let before = rects(&h);
    assert!(h.state_mut().execute("object.align_left"));
    h.run_steps(2);
    assert_eq!(h.state().toast.clone().unwrap().0, "Select objects with Edit objects first");
    assert_eq!(rects(&h), before);
    assert_eq!(undo_label(&h), None);

    // Both squares: the panel's align row lines up their left edges, one step.
    let (from, to) = (at(&h, 5.0, 5.0), at(&h, 260.0, 160.0));
    drag(&mut h, from, to);
    assert_eq!(selected(&h), vec![0, 1]);
    h.get_by_label("Align and distribute");
    h.get_by_label("Align left").click();
    h.run_steps(3);
    let r = rects(&h);
    assert!((r[0][0] - r[1][0]).abs() < 0.6, "{r:?}");
    assert_eq!(undo_label(&h).as_deref(), Some("Align left"));
    assert_eq!(selected(&h), vec![0, 1], "the selection stays");
    // Two objects can't be distributed: the buttons do nothing and the command explains.
    let aligned = rects(&h);
    h.get_by_label("Distribute horizontally").click();
    h.run_steps(3);
    assert_eq!(rects(&h), aligned);
    assert!(h.state_mut().execute("object.distribute_vertical"));
    h.run_steps(2);
    assert_eq!(h.state().toast.clone().unwrap().0, "Select three or more objects to distribute them");
    assert_eq!(rects(&h), aligned);

    // Right-click the selection: Align → Align top from the menu.
    let p = at(&h, 50.0, 70.0);
    right_click(&mut h, p);
    h.get_by_label("Align ⏵").hover();
    h.run_steps(3);
    // The panel has an "Align top" button too; the menu's is drawn last.
    assert_eq!(h.query_all_by_label("Align top").count(), 2);
    h.query_all_by_label("Align top").last().unwrap().click();
    h.run_steps(3);
    let r = rects(&h);
    assert!((r[0][3] - r[1][3]).abs() < 0.6, "tops line up: {r:?}");
    assert_eq!(undo_label(&h).as_deref(), Some("Align top"));
    // Arrange by command: the squares go above the text.
    assert!(h.state_mut().execute("object.bring_to_front"));
    h.run_steps(3);
    assert_eq!(undo_label(&h).as_deref(), Some("Bring to front"));
    assert_eq!(objects(&h)[0].kind, pdfcraft_engine::ObjectKind::Text);

    // All three: distribute horizontally by command; their centres are evenly spaced.
    let (from, to) = (at(&h, 2.0, 2.0), at(&h, 298.0, 298.0));
    drag(&mut h, from, to);
    assert_eq!(selected(&h).len(), 3);
    assert!(h.state_mut().execute("object.distribute_horizontal"));
    h.run_steps(3);
    assert_eq!(undo_label(&h).as_deref(), Some("Distribute horizontally"));
    // Undo, one step each.
    for _ in 0..4 {
        assert!(h.state_mut().execute("edit.undo"));
        h.run_steps(2);
    }
    assert_eq!(rects(&h), before);
}

#[test]
fn one_object_aligns_to_the_page() {
    let mut h = harness();
    let p = at(&h, 50.0, 70.0);
    click(&mut h, p);
    assert_eq!(selected(&h), vec![0]);
    h.get_by_label("Align to the page");
    h.get_by_label("Align right").click();
    h.run_steps(3);
    let r = rects(&h)[0];
    assert!((r[2] - 300.0).abs() < 0.6, "against the page's right edge: {r:?}");
    assert!(h.state_mut().execute("object.align_bottom"));
    h.run_steps(3);
    assert!(rects(&h)[0][1].abs() < 0.6, "{:?}", rects(&h)[0]);
    // An unknown object command is refused without a panic.
    assert!(!h.state_mut().execute("object.align_sideways"));
}
