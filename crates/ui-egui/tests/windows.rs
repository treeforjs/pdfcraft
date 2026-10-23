//! Multiple windows in the real shell (egui_kittest): View ▸ New window, moving tabs out and
//! back, dragging a tab out of the strip, one document in two windows kept in step on edits
//! and undo, closing a window with unsaved changes, the control channel's view of the windows
//! and hostile requests.
//!
//! kittest has no native windows, so egui shows each window inside the main one (an embedded
//! viewport, the same code path as a platform without multiple windows); the window model,
//! commands, rendering and prompts are the ones the desktop app uses.

use std::sync::{Arc, Mutex};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_engine::windows::WindowId;
use pdfcraft_engine::{DocId, Edit};
use pdfcraft_ui_egui::control::{ControlClient, Reply};
use pdfcraft_ui_egui::{CloseRequest, PdfCraftApp};
use serde_json::{Value, json};

fn fixture(n: usize) -> Vec<u8> {
    let mut objs: Vec<String> = vec!["<< /Type /Catalog /Pages 2 0 R >>".into()];
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
    objs.push(format!("<< /Type /Pages /Kids [{}] /Count {n} /MediaBox [0 0 200 300] >>", kids.join(" ")));
    objs.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());
    for i in 0..n {
        objs.push(format!("<< /Type /Page /Parent 2 0 R /Contents {} 0 R /Resources << /Font << /F1 3 0 R >> >> >>", 5 + 2 * i));
        let body = format!("BT /F1 24 Tf 20 150 Td (Page {}) Tj ET", i + 1);
        objs.push(format!("<< /Length {} >>\nstream\n{body}\nendstream", body.len()));
    }
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

/// a.pdf (3 pages) and b.pdf (2 pages), b.pdf active, with the control channel attached.
fn two_docs() -> (Harness<'static, PdfCraftApp>, ControlClient) {
    let slot: Arc<Mutex<Option<ControlClient>>> = Arc::default();
    let s = slot.clone();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        *s.lock().unwrap() = Some(app.attach_control(&cc.egui_ctx));
        app.open_bytes("a.pdf", None, fixture(3)).unwrap();
        app.open_bytes("b.pdf", None, fixture(2)).unwrap();
        app
    });
    h.run_steps(4);
    let client = slot.lock().unwrap().take().unwrap();
    (h, client)
}

fn call(h: &mut Harness<'static, PdfCraftApp>, c: &ControlClient, method: &str, params: Value) -> Reply {
    let rx = c.send(method, params);
    for _ in 0..30 {
        h.step();
        if let Ok(r) = rx.try_recv() {
            return r;
        }
    }
    Err("no reply".into())
}

fn key_of(h: &Harness<'static, PdfCraftApp>, name: &str) -> u64 {
    let app = h.state();
    app.views.iter().find(|v| app.session.get(v.id).is_some_and(|d| d.name == name)).map(|v| v.key).unwrap()
}

fn doc_of(h: &Harness<'static, PdfCraftApp>, name: &str) -> DocId {
    let app = h.state();
    app.views.iter().map(|v| v.id).find(|id| app.session.get(*id).is_some_and(|d| d.name == name)).unwrap()
}

fn active_name(h: &Harness<'static, PdfCraftApp>) -> Option<String> {
    let app = h.state();
    app.active_ids().and_then(|(_, id)| app.session.get(id)).map(|d| d.name.clone())
}

#[test]
fn a_second_window_shows_the_document_and_follows_edits_and_undo() {
    let (mut h, _c) = two_docs();
    // The second view starts where the first one is: b.pdf's last page.
    h.state_mut().set_option("page", "2").unwrap();
    h.run_steps(2);
    let b = doc_of(&h, "b.pdf");
    let first = key_of(&h, "b.pdf");
    assert!(h.state_mut().execute("window.new"));
    h.run_steps(4);
    assert_eq!(h.state().windows.len(), 1);
    let w = h.state().windows.windows()[0].clone();
    let second = w.active;
    assert_ne!(second, first, "a new view, not the same tab");
    assert_eq!(w.tabs[0].doc, b);
    // The main window keeps its tab, and the new window is drawn with the document.
    assert_eq!(active_name(&h).as_deref(), Some("b.pdf"));
    h.get_by_label("b.pdf — PdfCraft");
    assert_eq!(h.state().views.iter().filter(|v| v.id == b).count(), 2);
    let si = h.state().view_index(second).unwrap();
    assert!(h.state().views[si].viewport_rect().width() > 100.0, "the window drew its page view");
    assert_eq!(h.state().views[si].current, 1);
    // Delete that page from the main window: the second view follows (it moves to the page
    // that is now the last).
    assert!(h.state_mut().apply_edit(Edit::DeletePages { pages: vec![1] }));
    h.run_steps(3);
    let si = h.state().view_index(second).unwrap();
    assert_eq!(h.state().views[si].current, 0, "the second window shows the document as it is now");
    assert_eq!(h.state().session.get(b).unwrap().info.pages.len(), 1);
    // Undo from the main window: both views see two pages again.
    assert!(h.state_mut().execute("edit.undo"));
    h.run_steps(3);
    assert_eq!(h.state().session.get(b).unwrap().info.pages.len(), 2);
    // With unsaved changes, closing the main window's tab asks nothing: the document stays
    // open in the other window.
    assert!(h.state_mut().apply_edit(Edit::RotatePages { pages: vec![0], degrees: 90 }));
    let mi = h.state().view_index(first).unwrap();
    h.state_mut().request_close_tab(mi);
    h.run_steps(2);
    assert!(h.state().close_request.is_none(), "the document stays open in the other window");
    assert!(h.state().session.get(b).is_some_and(|d| d.dirty));
    assert_eq!(active_name(&h).as_deref(), Some("a.pdf"), "the main window shows its other tab");
    assert_eq!(h.state().windows.len(), 1);
}

#[test]
fn tabs_move_to_a_new_window_and_back() {
    let (mut h, c) = two_docs();
    let b = key_of(&h, "b.pdf");
    assert!(h.state_mut().execute("window.move_tab"));
    h.run_steps(4);
    assert_eq!(h.state().windows.len(), 1);
    let w = h.state().windows.window_of(b);
    assert!(!w.is_main());
    assert_eq!(active_name(&h).as_deref(), Some("a.pdf"), "the main window shows the tab beside it");
    // The main strip lists a.pdf only; the window shows b.pdf.
    h.get_by_label("b.pdf — PdfCraft");
    let state = call(&mut h, &c, "ui.state", json!({})).unwrap();
    let windows = &state["windows"]["windows"];
    assert_eq!(windows[0]["tabs"].as_array().unwrap().len(), 1, "{state}");
    assert_eq!(windows[1]["id"], w.0);
    assert_eq!(windows[1]["active_view"], b);
    assert_eq!(state["documents"][1]["window"], w.0);
    // Moving the only tab of a window into a new one is refused, and nothing changes.
    assert!(h.state_mut().move_tab_to_window(b, None).is_err());
    // Back with the window's own button: the window closes and the tab is active again.
    h.get_by_label("Move tab to main window").click();
    h.run_steps(4);
    assert!(h.state().windows.is_empty());
    assert_eq!(active_name(&h).as_deref(), Some("b.pdf"));
    // The same through the control channel.
    call(&mut h, &c, "ui.set", json!({ "key": "window-move-tab", "value": format!("{b}:new") })).unwrap();
    h.run_steps(3);
    let w = h.state().windows.window_of(b);
    assert!(!w.is_main());
    call(&mut h, &c, "ui.set", json!({ "key": "window-move-tab", "value": format!("{b}:main") })).unwrap();
    h.run_steps(3);
    assert!(h.state().windows.is_empty());
}

#[test]
fn dragging_a_tab_out_of_the_strip_opens_a_window() {
    let (mut h, _c) = two_docs();
    let b = key_of(&h, "b.pdf");
    let tab = h.get_by_label("b.pdf").rect();
    let start = tab.center();
    h.hover_at(start);
    h.run_steps(1);
    h.drag_at(start);
    h.run_steps(1);
    for step in 1..=10 {
        h.hover_at(start + egui::vec2(0.0, 30.0 * step as f32));
        h.run_steps(1);
    }
    h.get_by_label("Release to open in a new window");
    h.drop_at(start + egui::vec2(0.0, 300.0));
    h.run_steps(4);
    assert!(!h.state().windows.window_of(b).is_main(), "b.pdf moved to a window of its own");
    assert_eq!(active_name(&h).as_deref(), Some("a.pdf"));
    // A short drag along the strip leaves a tab where it is.
    let a = key_of(&h, "a.pdf");
    let tab = h.get_by_label("a.pdf").rect();
    h.hover_at(tab.center());
    h.run_steps(1);
    h.drag_at(tab.center());
    h.run_steps(1);
    h.hover_at(tab.center() + egui::vec2(40.0, 4.0));
    h.run_steps(1);
    h.drop_at(tab.center() + egui::vec2(40.0, 4.0));
    h.run_steps(3);
    assert!(h.state().windows.window_of(a).is_main());
}

#[test]
fn closing_a_window_with_unsaved_changes_asks_first() {
    let (mut h, c) = two_docs();
    let b = key_of(&h, "b.pdf");
    let doc = doc_of(&h, "b.pdf");
    assert!(h.state_mut().apply_edit(Edit::RotatePages { pages: vec![0], degrees: 90 }));
    let w = h.state_mut().move_tab_to_window(b, None).unwrap();
    h.run_steps(3);
    // A clean second view in the same window closes at once; the unsaved one asks.
    let a = key_of(&h, "a.pdf");
    h.state_mut().move_tab_to_window(a, Some(w)).unwrap();
    h.run_steps(2);
    call(&mut h, &c, "ui.set", json!({ "key": "window-close", "value": w.0.to_string() })).unwrap();
    h.run_steps(3);
    assert_eq!(h.state().close_request, Some(CloseRequest::Window(w)));
    assert!(h.state().session.docs().iter().all(|d| d.name != "a.pdf"), "the clean document closed");
    h.get_by_label("Save changes to “b.pdf” before closing?");
    // Cancel keeps the window and the document.
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert!(h.state().close_request.is_none());
    assert!(h.state().windows.get(w).is_some());
    // Don't save: the document closes, and the window with it.
    h.state_mut().request_close_window(w);
    h.run_steps(2);
    h.get_by_label("Don't save").click();
    h.run_steps(3);
    assert!(h.state().windows.is_empty());
    assert!(h.state().session.get(doc).is_none());
    assert!(h.state().views.is_empty());
}

#[test]
fn stale_and_hostile_window_requests_are_refused() {
    let (mut h, c) = two_docs();
    let b = key_of(&h, "b.pdf");
    for (key, value) in [
        ("window-move-tab", "999999:main".to_string()),
        ("window-move-tab", format!("{b}:77")),
        ("window-move-tab", format!("{b}:-1")),
        ("window-move-tab", format!("{b}:4294967296")),
        ("window-move-tab", "nonsense".to_string()),
        ("window-move-tab", ":".to_string()),
        ("window-new", "18446744073709551615".to_string()),
        ("window-new", "-3".to_string()),
        ("window-close", "0".to_string()),
        ("window-close", "5".to_string()),
        ("window-focus", "9".to_string()),
        ("window-focus", "x".to_string()),
    ] {
        let r = call(&mut h, &c, "ui.set", json!({ "key": key, "value": value }));
        assert!(r.is_err(), "{key}={value} should be refused");
    }
    assert!(h.state().windows.is_empty());
    // A tab that closed while a request for it was on its way.
    let a = key_of(&h, "a.pdf");
    let ai = h.state().view_index(a).unwrap();
    h.state_mut().close_tab(ai);
    assert!(h.state_mut().move_tab_to_window(a, None).is_err());
    assert!(h.state_mut().new_window_for(a).is_err());
    // Commands that don't apply say why and change nothing.
    assert!(!h.state_mut().execute("window.close"));
    assert!(!h.state_mut().execute("window.move_to_main"));
    // The number of windows is capped.
    for _ in 0..pdfcraft_engine::windows::MAX_WINDOWS {
        h.state_mut().new_window_for(b).unwrap();
    }
    assert!(h.state_mut().new_window_for(b).is_err());
    assert!(!h.state_mut().execute("window.new"));
    h.run_steps(3);
    assert_eq!(h.state().windows.len(), pdfcraft_engine::windows::MAX_WINDOWS);
    // Eight views of b.pdf share the allowance; closing the document's last tab everywhere
    // leaves no window behind.
    while let Some(i) = h.state().views.iter().rposition(|v| v.key != b) {
        h.state_mut().close_tab(i);
    }
    h.run_steps(2);
    assert!(h.state().windows.is_empty());
    assert!(h.state_mut().focus_window(WindowId(3)).is_err());
}
