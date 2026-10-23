//! Edit a PDF ▸ Link: drawing a link, Link Properties, editing and deleting links.

use egui::Pos2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_engine::LinkAction;
use pdfcraft_ui_egui::{Dialog, PdfCraftApp};

fn harness() -> Harness<'static, PdfCraftApp> {
    // 60 fps steps, so two clicks fall inside egui's double-click window.
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_step_dt(1.0 / 60.0).build_eframe(|_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("form.pdf", None, include_bytes!("data/form.pdf").to_vec()).unwrap();
        app.set_option("zoom", "150").unwrap();
        app
    });
    h.run_steps(6);
    h
}

fn at(h: &Harness<'static, PdfCraftApp>, x: f32, y: f32) -> Pos2 {
    let r = h.state().views[0].page_screen_rect(0).unwrap();
    let k = r.width() / 300.0;
    r.min + egui::vec2(x * k, y * k)
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

fn links(h: &Harness<'static, PdfCraftApp>) -> Vec<pdfcraft_engine::LinkItem> {
    let s = h.state();
    s.session.get(s.views[0].id).unwrap().links.clone()
}

#[test]
fn drawing_editing_and_deleting_links() {
    let mut h = harness();
    assert!(h.state_mut().execute("edit.link"));
    let (a, b) = (at(&h, 40.0, 300.0), at(&h, 160.0, 320.0));
    drag(&mut h, a, b);
    assert_eq!(h.state().dialog, Some(Dialog::LinkProps));
    h.get_by_label("Create Link");
    h.state_mut().link_draft.as_mut().unwrap().url = "https://example.org".into();
    h.get_by_label("OK").click();
    h.run_steps(3);
    let l = links(&h);
    assert_eq!(l.len(), 1);
    assert_eq!(l[0].action, LinkAction::Uri("https://example.org".into()));
    // Double-click opens Link Properties; switch to a page action.
    let c = at(&h, 100.0, 310.0);
    h.hover_at(c);
    h.run_steps(1);
    for _ in 0..2 {
        h.drag_at(c);
        h.step();
        h.drop_at(c);
        h.step();
    }
    h.run_steps(2);
    assert_eq!(h.state().dialog, Some(Dialog::LinkProps));
    h.get_by_label("Link Properties");
    {
        let d = h.state_mut().link_draft.as_mut().unwrap();
        d.kind = pdfcraft_ui_egui::LinkKind::Page;
        d.target = 1;
    }
    h.get_by_label("OK").click();
    h.run_steps(3);
    assert_eq!(links(&h)[0].action, LinkAction::Page(0));
    // Delete the selected link.
    h.key_press(egui::Key::Delete);
    h.run_steps(3);
    assert!(links(&h).is_empty());
}

/// Draw a new link over the page area (x0, y0)–(x1, y1) and choose Go to a page view.
fn new_page_link(h: &mut Harness<'static, PdfCraftApp>, x0: f32, y0: f32, x1: f32, y1: f32) {
    assert!(h.state_mut().execute("edit.link"));
    let (a, b) = (at(h, x0, y0), at(h, x1, y1));
    drag(h, a, b);
    assert_eq!(h.state().dialog, Some(Dialog::LinkProps));
    h.get_by_label("Go to a page view").click();
    h.run_steps(2);
}

#[test]
fn links_go_to_a_zoom_the_current_view_or_a_named_destination() {
    let mut h = harness();
    // Fit width from the Zoom list.
    new_page_link(&mut h, 40.0, 300.0, 160.0, 320.0);
    h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Zoom").click();
    h.run_steps(2);
    h.get_by_label("Fit width").click();
    h.run_steps(2);
    h.get_by_label("OK").click();
    h.run_steps(3);
    assert_eq!(links(&h)[0].action, LinkAction::View(pdfcraft_engine::DestView { page: 0, fit: pdfcraft_engine::DestFit::FitH { top: None } }));

    // Use current view: the page as it is on screen, at 150 %.
    new_page_link(&mut h, 40.0, 200.0, 160.0, 220.0);
    h.get_by_label("Use current view").click();
    h.run_steps(2);
    let v = h.state().link_draft.as_ref().unwrap().view;
    assert!(matches!(v, pdfcraft_ui_egui::ViewFit::Exact(pdfcraft_engine::DestFit::Xyz { .. })), "{v:?}");
    h.get_by_label("OK").click();
    h.run_steps(3);
    let made = links(&h);
    let view = made.iter().find_map(|l| match &l.action {
        LinkAction::View(v) if matches!(v.fit, pdfcraft_engine::DestFit::Xyz { .. }) => Some(*v),
        _ => None,
    });
    let Some(pdfcraft_engine::DestView { page: 0, fit: pdfcraft_engine::DestFit::Xyz { zoom: Some(z), left: Some(_), top: Some(_) } }) = view else {
        panic!("{made:?}")
    };
    assert!((z - 1.5).abs() < 1e-6, "{z}");

    // No named destinations yet: the dialog says so and OK waits.
    new_page_link(&mut h, 40.0, 100.0, 160.0, 120.0);
    h.get_by_label("Go to a named destination").click();
    h.run_steps(2);
    h.get_by_label("This document has no named destinations.");
    assert!(h.query_by(|n| n.label().as_deref() == Some("OK") && n.is_disabled()).is_some());
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    let id = h.state().views[0].id;
    let dest = pdfcraft_engine::DestView { page: 0, fit: pdfcraft_engine::DestFit::Fit };
    h.state_mut().session.apply(id, pdfcraft_engine::Edit::SetDestination { name: "chapter-2".into(), view: dest }).unwrap();
    h.run_steps(2);
    new_page_link(&mut h, 40.0, 100.0, 160.0, 120.0);
    h.get_by_label("Go to a named destination").click();
    h.run_steps(2);
    // The list offers the document's destinations, the first chosen to start with.
    h.get_by(|n| n.role() == egui::accesskit::Role::ComboBox && n.value().as_deref() == Some("chapter-2")).click();
    h.run_steps(2);
    h.query_all_by_label("chapter-2").last().unwrap().click();
    h.run_steps(2);
    h.get_by_label("OK").click();
    h.run_steps(3);
    assert!(links(&h).iter().any(|l| l.action == LinkAction::Named("chapter-2".into())), "{:?}", links(&h));
}
