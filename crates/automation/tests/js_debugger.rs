//! The JavaScript debugger end to end through the tool table: every script listed with where it
//! lives, read, debugged with breakpoints, steps and watches, errors with lines, the JavaScript
//! preference, and hostile scripts and arguments.

use pdfcraft_automation::{Automation, Content};
use serde_json::{Value, json};

fn setup(name: &str) -> (Automation, u64, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("pdfcraft-jsdebug-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let blank = pdfcraft_engine::Session::new().create_blank(300.0, 300.0, 2).unwrap();
    std::fs::write(dir.join("a.pdf"), blank.as_slice()).unwrap();
    let mut a = Automation::new().with_root(&dir).unwrap().with_clock(|| 1_700_000_000);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "form_add_field", json!({ "doc": doc, "page": 1, "type": "text", "rect": [20, 20, 220, 42], "name": "qty" }));
    ok(&mut a, "form_add_field", json!({ "doc": doc, "page": 1, "type": "text", "rect": [20, 60, 220, 82], "name": "total" }));
    (a, doc, dir)
}

fn ok(a: &mut Automation, tool: &str, args: Value) -> Value {
    match a.call(tool, &args) {
        Ok(mut c) => match c.remove(0) {
            Content::Json(v) => v,
            other => panic!("{tool}: expected JSON, got {other:?}"),
        },
        Err(e) => panic!("{tool} {args}: {e}"),
    }
}

fn err(a: &mut Automation, tool: &str, args: Value) -> String {
    match a.call(tool, &args) {
        Ok(c) => panic!("{tool} {args}: expected an error, got {c:?}"),
        Err(e) => e.to_string(),
    }
}

const CALC: &str = "var q = Number(getField('qty').value);\nvar t = double(q);\nevent.value = t;";

#[test]
fn scripts_are_listed_read_and_debugged() {
    let (mut a, doc, dir) = setup("all");
    ok(&mut a, "js_set_document_script", json!({ "doc": doc, "name": "init", "script": "function double(n) {\n  var r = n * 2;\n  return r;\n}" }));
    ok(&mut a, "form_set_script", json!({ "doc": doc, "field": "total", "event": "calculate", "script": CALC }));
    ok(&mut a, "js_set_page_action", json!({ "doc": doc, "page": 2, "trigger": "page_open", "script": "console.println('page 2');" }));
    ok(&mut a, "link_add", json!({ "doc": doc, "page": 1, "rect": [20, 120, 120, 150], "javascript": "app.alert('link');" }));
    ok(&mut a, "js_set_document_action", json!({ "doc": doc, "trigger": "will_save", "script": "console.println('saving');" }));

    let list = ok(&mut a, "js_scripts", json!({ "doc": doc }));
    let ids: Vec<&str> = list["scripts"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["document:init", "document_action:will_save", "page:2:page_open", "field:calculate:total", "link:1:3"]);
    assert_eq!(list["truncated"], false);
    let only = ok(&mut a, "js_scripts", json!({ "doc": doc, "kind": "field" }));
    assert_eq!(only["scripts"].as_array().unwrap().len(), 1, "{only}");
    assert_eq!(only["scripts"][0]["location"], "Field total: calculate");

    let s = ok(&mut a, "js_script", json!({ "doc": doc, "id": "field:calculate:total" }));
    assert_eq!(s["script"], CALC);
    assert_eq!(s["breakable"], json!([1, 2, 3]));

    // Stop at line 2 of the calculate script, as it runs for its field.
    ok(&mut a, "form_fill", json!({ "doc": doc, "values": { "qty": "4" } }));
    let p = ok(
        &mut a,
        "js_debug",
        json!({ "doc": doc, "id": "field:calculate:total", "breakpoints": [2], "watches": ["q", "event.name", "event.target.name"] }),
    );
    assert_eq!(p["status"], "paused", "{p}");
    assert_eq!(p["paused"]["line"], 2);
    let w = &p["paused"]["watches"];
    assert_eq!(
        (w[0]["value"].clone(), w[1]["value"].clone(), w[2]["value"].clone()),
        (json!("4"), json!("\"Calculate\""), json!("\"total\"")),
        "{p}"
    );
    // Into the document-level function: the run doesn't stop inside it (only the debugged
    // script has probes), so it goes on to line 3.
    let resume = p["paused"]["resume"].clone();
    let p2 = ok(&mut a, "js_debug", json!({ "doc": doc, "id": "field:calculate:total", "step": "into", "resume": resume, "watches": ["t"] }));
    assert_eq!(p2["paused"]["line"], 3, "{p2}");
    assert_eq!(p2["paused"]["watches"][0]["value"], "8");
    let done = ok(&mut a, "js_debug", json!({ "doc": doc, "id": "field:calculate:total", "resume": p2["paused"]["resume"] }));
    assert_eq!((done["status"].clone(), done["value"].clone()), (json!("finished"), json!("8")), "{done}");

    // A typed script: console output, the result, and errors with their line.
    let r = ok(&mut a, "js_debug", json!({ "doc": doc, "script": "console.println(numFields);\n6 * 7" }));
    assert_eq!((r["console"].clone(), r["result"].clone()), (json!(["2"]), json!("42")), "{r}");
    let r = ok(&mut a, "js_debug", json!({ "doc": doc, "script": "var a = 1;\n\nnull.x;" }));
    assert_eq!(r["error_at"]["line"], 3, "{r}");
    assert_eq!(r["error_at"]["script"], "script");
    let r = ok(&mut a, "js_debug", json!({ "doc": doc, "script": "double(null.x)" }));
    assert!(r["error"].as_str().unwrap().contains("TypeError"), "{r}");
    // An error inside a document-level script names it.
    let r = ok(&mut a, "js_debug", json!({ "doc": doc, "script": "double({ valueOf: function () { throw new Error('inner'); } })" }));
    assert!(r["error"].as_str().unwrap().contains("inner"), "{r}");
    // A finished run applies its changes, like the console; a paused one doesn't.
    ok(&mut a, "js_debug", json!({ "doc": doc, "script": "getField('qty').value = 9;\ngetField('total').value = 1;", "breakpoints": [2] }));
    assert_eq!(ok(&mut a, "form_fields", json!({ "doc": doc }))["fields"][0]["value"], "4");
    ok(&mut a, "js_debug", json!({ "doc": doc, "script": "getField('qty').value = 9;" }));
    assert_eq!(ok(&mut a, "form_fields", json!({ "doc": doc }))["fields"][0]["value"], "9");

    // JavaScript off: the debugger refuses to run, listing still works.
    ok(&mut a, "js_enabled", json!({ "enabled": false }));
    assert!(err(&mut a, "js_debug", json!({ "doc": doc, "script": "1" })).contains("turned off"));
    ok(&mut a, "js_scripts", json!({ "doc": doc }));
    ok(&mut a, "js_enabled", json!({ "enabled": true }));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn hostile_scripts_and_arguments_are_refused_or_bounded() {
    let (mut a, doc, dir) = setup("hostile");
    // Runaway scripts stop on the step budget or the clock, with where they were.
    let r = ok(
        &mut a,
        "js_debug",
        json!({ "doc": doc, "script": "var n = 0;\nfunction f() { for (var i = 0; i < 900000; i++) n++; }\nfor (var j = 0; j < 900000; j++) f();", "max_steps": 100000 }),
    );
    assert_eq!(r["status"], "stopped", "{r}");
    assert!(r["error_at"]["line"].as_u64().is_some(), "{r}");
    for s in [
        "var s = 'x'; while (true) s = s + s;",
        "'ab'.repeat(1 << 30)",
        "new Array(4294967295).fill(0)",
        "Math.max.apply(null, { length: 1 << 30 })",
        "function f() { return f(); } f();",
        &format!("var a = {}1{};", "[".repeat(50_000), "]".repeat(50_000)),
        "\u{0}\u{feff}((((",
    ] {
        let r = ok(&mut a, "js_debug", json!({ "doc": doc, "script": s, "timeout_ms": 5000 }));
        assert!(r["error"].is_string(), "{s:.40}: {r}");
    }
    // Bad arguments say what's allowed.
    for (args, want) in [
        (json!({ "doc": doc }), "script"),
        (json!({ "doc": doc, "id": "nonsense" }), "not a script id"),
        (json!({ "doc": doc, "id": "document:nope" }), "no script"),
        (json!({ "doc": doc, "id": "page:0:page_open" }), "not a script id"),
        (json!({ "doc": doc, "id": "link:99999999999999999999:1" }), "not a script id"),
        (json!({ "doc": doc, "script": "1", "step": "sideways" }), "step"),
        (json!({ "doc": doc, "script": "1", "breakpoints": [0] }), "breakpoints"),
        (json!({ "doc": doc, "script": "1", "breakpoints": [-5] }), "breakpoints"),
        (json!({ "doc": doc, "script": "1", "max_steps": 0 }), "max_steps"),
        (json!({ "doc": doc, "script": "1", "timeout_ms": 1 }), "timeout_ms"),
        (json!({ "doc": doc, "script": "1", "resume": { "hit": -1 } }), "resume"),
        (json!({ "doc": doc, "script": "1", "watches": (0..40).map(|i| i.to_string()).collect::<Vec<_>>() }), "watches"),
    ] {
        let e = err(&mut a, "js_debug", args.clone());
        assert!(e.contains(want), "{args}: {e}");
    }
    // Huge or strange numbers in ids and resume points never panic.
    let r = ok(&mut a, "js_debug", json!({ "doc": doc, "script": "1;\n2;", "step": "over", "resume": { "hit": u64::MAX, "depth": u64::MAX } }));
    assert_eq!(r["status"], "finished");
    let _ = err(&mut a, "js_script", json!({ "doc": doc, "id": "bookmark:1.2.99999" }));
    let _ = err(&mut a, "js_script", json!({ "doc": doc, "id": "annotation:1:1:nope" }));
    let _ = std::fs::remove_dir_all(dir);
}
