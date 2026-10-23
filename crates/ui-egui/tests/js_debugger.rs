//! The JavaScript debugger in the app (egui_kittest): the script list, breakpoints, stepping,
//! watches, the console, errors with lines, the control channel, the JavaScript preference and
//! hostile scripts.

use std::sync::{Arc, Mutex};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::control::ControlClient;
use pdfcraft_ui_egui::js_debug_ui::{LogKind, RunStatus};
use pdfcraft_ui_egui::{Dialog, PdfCraftApp};
use serde_json::{Value, json};

/// A form: qty and total (whose calculate script doubles qty with a document-level function),
/// and a link that runs a script.
fn doc() -> Vec<u8> {
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R /Names << /JavaScript << /Names [(init) 8 0 R] >> >> /AcroForm << /Fields [4 0 R 5 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 6 0 R >> >> >> >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 300 300] >>",
        "<< /Type /Page /Parent 2 0 R /Annots [4 0 R 5 0 R 7 0 R] >>",
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (qty) /V (4) /Rect [20 250 280 270] /P 3 0 R /F 4 >>",
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (total) /Rect [20 200 280 220] /P 3 0 R /F 4 /AA << /C 9 0 R >> >>",
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
        "<< /Type /Annot /Subtype /Link /Rect [20 100 120 130] /P 3 0 R /A << /S /JavaScript /JS (app.alert\\('hi'\\);) >> >>",
        "<< /S /JavaScript /JS (function double\\(n\\) { return n * 2; }) >>",
        "<< /S /JavaScript /JS (var q = Number\\(getField\\('qty'\\).value\\);\\nvar t = double\\(q\\);\\nevent.value = t;) >>",
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

fn harness() -> (Harness<'static, PdfCraftApp>, ControlClient) {
    let slot: Arc<Mutex<Option<ControlClient>>> = Arc::default();
    let s = slot.clone();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        *s.lock().unwrap() = Some(app.attach_control(&cc.egui_ctx));
        app.open_bytes("form.pdf", None, doc()).unwrap();
        app
    });
    h.run_steps(4);
    let client = slot.lock().unwrap().take().unwrap();
    (h, client)
}

fn state(h: &mut Harness<'static, PdfCraftApp>, c: &ControlClient) -> Value {
    let rx = c.send("ui.state", json!({}));
    for _ in 0..30 {
        h.step();
        if let Ok(r) = rx.try_recv() {
            return r.unwrap();
        }
    }
    panic!("no reply");
}

fn click(h: &mut Harness<'static, PdfCraftApp>, label: &str) {
    h.run_steps(2);
    h.get_by_label(label).click();
    h.run_steps(2);
}

#[test]
fn breakpoints_steps_watches_and_the_console() {
    let (mut h, c) = harness();
    assert!(h.state_mut().execute("tools.js_debugger"));
    h.run_steps(3);
    h.get_by_label("JavaScript Debugger");
    assert_eq!(h.state().dialog, Some(Dialog::JsDebugger));
    let s = state(&mut h, &c);
    assert_eq!(s["js_debugger"]["scripts"], json!(["document:init", "field:calculate:total", "link:1:3"]), "{s}");

    // The calculate script, a breakpoint on line 2 and a watch.
    h.state_mut().set_option("js-debugger-script", "field:calculate:total").unwrap();
    h.state_mut().set_option("js-debugger-breakpoints", "2").unwrap();
    h.state_mut().set_option("js-debugger-watches", "q").unwrap();
    assert_eq!(h.state().js_debugger.breakable, [1, 2, 3]);
    click(&mut h, "Run");
    assert_eq!(h.state().js_debugger.status, RunStatus::Paused(2));
    let p = h.state().js_debugger.paused.clone().unwrap();
    assert_eq!(p.watches[0].value.as_deref(), Some("4"));
    // Evaluate in the paused scope.
    h.state_mut().js_debugger.input = "q * 10".into();
    click(&mut h, "Evaluate");
    assert_eq!(h.state().js_debugger.log.last().unwrap(), &(LogKind::Result, "40".to_string()));
    click(&mut h, "Step over");
    assert_eq!(h.state().js_debugger.status, RunStatus::Paused(3));
    let s = state(&mut h, &c);
    assert_eq!(s["js_debugger"]["paused"]["line"], 3, "{s}");
    click(&mut h, "Continue");
    assert_eq!(h.state().js_debugger.status, RunStatus::Finished);

    // Clicking a line number toggles a breakpoint on a line that takes one.
    h.state_mut().js_debugger.toggle_breakpoint(3);
    assert!(h.state().js_debugger.breakpoints.contains(&3));
    h.state_mut().js_debugger.toggle_breakpoint(99);
    assert!(!h.state().js_debugger.breakpoints.contains(&99));

    // The scratch script: console output, the result, and an error with its line.
    h.state_mut().set_option("js-debugger-script", "scratch").unwrap();
    h.state_mut().js_debugger.source = "console.println(numFields);\n6 * 7".into();
    click(&mut h, "Run");
    let log = h.state().js_debugger.log.clone();
    assert!(log.contains(&(LogKind::Output, "2".into())) && log.contains(&(LogKind::Result, "42".into())), "{log:?}");
    h.state_mut().js_debugger.source = "var a = 1;\nnull.x;".into();
    click(&mut h, "Run");
    assert_eq!(h.state().js_debugger.status, RunStatus::Error(Some(2)));
    // A finished run changes the document, like the console.
    h.state_mut().js_debugger.input = "getField('qty').value = 7".into();
    click(&mut h, "Evaluate");
    let id = h.state().active_ids().unwrap().1;
    assert_eq!(h.state().session.get(id).unwrap().form.iter().find(|f| f.name == "qty").unwrap().value, ["7"]);

    // JavaScript off: nothing runs.
    h.state_mut().session.set_javascript(false);
    h.state_mut().js_debugger.source = "getField('qty').value = 1".into();
    click(&mut h, "Run");
    assert_eq!(h.state().js_debugger.log.last().unwrap().0, LogKind::Info);
    assert_eq!(h.state().session.get(id).unwrap().form.iter().find(|f| f.name == "qty").unwrap().value, ["7"]);
    h.state_mut().session.set_javascript(true);
    click(&mut h, "Close");
    assert_eq!(h.state().dialog, None);
}

#[test]
fn hostile_scripts_never_take_the_app_down() {
    let (mut h, _c) = harness();
    h.state_mut().set_option("dialog", "js-debugger").unwrap();
    // The window's first frames only size it.
    h.run_steps(4);
    h.state_mut().set_option("js-debugger-script", "scratch").unwrap();
    h.state_mut().js_debugger.source = "1+1".to_string();
    click(&mut h, "Run");
    assert_eq!(h.state().js_debugger.status, RunStatus::Finished);
    for s in [
        "var s = 'x'; while (true) s = s + s;",
        "function f() { for (var i = 0; i < 900000; i++) {} }\nfor (var j = 0; j < 900000; j++) f();",
        "'ab'.repeat(1 << 30)",
        &format!("{}1{}", "(".repeat(20_000), ")".repeat(20_000)),
        "\u{0}((((",
    ] {
        h.state_mut().js_debugger.source = s.to_string();
        click(&mut h, "Run");
        assert!(
            matches!(h.state().js_debugger.status, RunStatus::Error(_) | RunStatus::Stopped),
            "{s:.30}: {:?} {:?}",
            h.state().js_debugger.status,
            h.state().js_debugger.log
        );
    }
    // Bad control values are refused with a reason.
    assert!(h.state_mut().set_option("js-debugger-script", "nope:1").is_err());
    assert!(h.state_mut().set_option("js-debugger-breakpoints", "x,-1").is_err());
    h.state_mut().set_option("js-debugger-breakpoints", "4294967295").unwrap();
}
