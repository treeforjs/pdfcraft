//! The UI control channel (M3.9): an agent can see the widget tree, click, type, press keys,
//! run commands, change view options and take screenshots of the running app.

use std::sync::{Arc, Mutex};

use egui_kittest::Harness;
use printcraft_ui_egui::PrintCraftApp;
use printcraft_ui_egui::control::{ControlClient, Reply};
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

fn harness() -> (Harness<'static, PrintCraftApp>, ControlClient) {
    let slot: Arc<Mutex<Option<ControlClient>>> = Arc::default();
    let s = slot.clone();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |cc| {
        let mut app = PrintCraftApp::new();
        *s.lock().unwrap() = Some(app.attach_control(&cc.egui_ctx));
        app.open_bytes("doc.pdf", None, fixture(5)).unwrap();
        app
    });
    h.run_steps(4);
    let client = slot.lock().unwrap().take().unwrap();
    (h, client)
}

/// Send a request and run frames until it is answered.
fn call(h: &mut Harness<'static, PrintCraftApp>, c: &ControlClient, method: &str, params: Value) -> Reply {
    let rx = c.send(method, params);
    for _ in 0..30 {
        h.step();
        if let Ok(r) = rx.try_recv() {
            return r;
        }
    }
    panic!("{method}: no reply after 30 frames");
}

fn ok(h: &mut Harness<'static, PrintCraftApp>, c: &ControlClient, method: &str, params: Value) -> Value {
    call(h, c, method, params).unwrap_or_else(|e| panic!("{method}: {e}"))
}

#[test]
fn state_and_view_options() {
    let (mut h, c) = harness();
    let s = ok(&mut h, &c, "ui.state", json!({}));
    assert_eq!(s["documents"][0]["name"], "doc.pdf");
    assert_eq!(s["documents"][0]["pages"], 5);
    assert_eq!(s["active"]["page"], 1);
    ok(&mut h, &c, "ui.set", json!({ "key": "page", "value": 4 }));
    h.run_steps(3);
    assert_eq!(ok(&mut h, &c, "ui.state", json!({}))["active"]["page"], 4);
    ok(&mut h, &c, "ui.set", json!({ "key": "panel", "value": "bookmarks" }));
    assert_eq!(ok(&mut h, &c, "ui.state", json!({}))["right_panel"], "Bookmarks");
    assert!(call(&mut h, &c, "ui.set", json!({ "key": "panel", "value": "nonsense" })).is_err());
}

#[test]
fn inspect_and_click_by_label_and_id() {
    let (mut h, c) = harness();
    let found = ok(&mut h, &c, "ui.inspect", json!({ "query": "read" }));
    let read = found["widgets"].as_array().unwrap().iter().find(|w| w["label"] == "Read" && w["clickable"] == true).cloned().expect("a Read tab");
    let rect = read["rect"].as_array().unwrap();
    assert!(rect[2].as_f64().unwrap() > rect[0].as_f64().unwrap());

    ok(&mut h, &c, "ui.click", json!({ "label": "Read" }));
    assert_eq!(ok(&mut h, &c, "ui.state", json!({}))["mode"], "Read");

    let all = ok(&mut h, &c, "ui.inspect", json!({ "query": "all tools" }));
    let id = all["widgets"].as_array().unwrap().iter().find(|w| w["label"] == "All tools" && w["clickable"] == true).unwrap()["id"].clone();
    ok(&mut h, &c, "ui.click", json!({ "id": id }));
    assert_eq!(ok(&mut h, &c, "ui.state", json!({}))["mode"], "AllTools");

    // Clicking a point works too (here: the Read tab's centre).
    let [x0, y0, x1, y1] = [0, 1, 2, 3].map(|i| rect[i].as_f64().unwrap());
    ok(&mut h, &c, "ui.click", json!({ "x": (x0 + x1) / 2.0, "y": (y0 + y1) / 2.0 }));
    assert_eq!(ok(&mut h, &c, "ui.state", json!({}))["mode"], "Read");

    let err = call(&mut h, &c, "ui.click", json!({ "label": "No such button" })).unwrap_err();
    assert!(err.contains("no enabled clickable widget"), "{err}");
    assert!(call(&mut h, &c, "ui.click", json!({ "id": 12345 })).is_err());
}

#[test]
fn commands_keys_and_typing() {
    let (mut h, c) = harness();
    let list = ok(&mut h, &c, "ui.commands", json!({}));
    let undo = list["commands"].as_array().unwrap().iter().find(|x| x["id"] == "edit.undo").unwrap().clone();
    assert_eq!(undo["enabled"], false);
    assert!(call(&mut h, &c, "ui.command", json!({ "id": "edit.undo" })).unwrap_err().contains("disabled"));
    assert!(call(&mut h, &c, "ui.command", json!({ "id": "nope" })).is_err());

    ok(&mut h, &c, "ui.command", json!({ "id": "page.rotate" }));
    h.run_steps(2);
    let list = ok(&mut h, &c, "ui.commands", json!({}));
    assert_eq!(list["commands"].as_array().unwrap().iter().find(|x| x["id"] == "edit.undo").unwrap()["enabled"], true);

    // ⌘K opens the palette; typing filters it; Escape closes it.
    ok(&mut h, &c, "ui.key", json!({ "key": "K", "modifiers": ["command"] }));
    assert_eq!(ok(&mut h, &c, "ui.state", json!({}))["palette_open"], true);
    ok(&mut h, &c, "ui.type", json!({ "text": "split" }));
    let hits = ok(&mut h, &c, "ui.inspect", json!({ "query": "split document" }));
    assert!(hits["count"].as_u64().unwrap() >= 1, "{hits}");
    ok(&mut h, &c, "ui.key", json!({ "key": "Escape" }));
    assert_eq!(ok(&mut h, &c, "ui.state", json!({}))["palette_open"], false);

    assert!(call(&mut h, &c, "ui.key", json!({ "key": "NotAKey" })).is_err());
    assert!(call(&mut h, &c, "ui.frobnicate", json!({})).unwrap_err().contains("unknown method"));
}

#[test]
fn screenshots_of_window_and_region() {
    let (mut h, c) = harness();
    let shot = ok(&mut h, &c, "ui.screenshot", json!({}));
    let ppp = shot["pixels_per_point"].as_f64().unwrap();
    assert_eq!(shot["width"].as_f64().unwrap(), (1400.0 * ppp).round());
    use base64::Engine as _;
    let png = base64::engine::general_purpose::STANDARD.decode(shot["png_base64"].as_str().unwrap()).unwrap();
    assert_eq!(&png[1..4], b"PNG");
    let region = ok(&mut h, &c, "ui.screenshot", json!({ "region": [10, 20, 110, 70] }));
    assert_eq!((region["width"].as_f64().unwrap(), region["height"].as_f64().unwrap()), ((100.0 * ppp).round(), (50.0 * ppp).round()));
    assert!(call(&mut h, &c, "ui.screenshot", json!({ "region": [5, 5, 1, 1] })).is_err());
}
