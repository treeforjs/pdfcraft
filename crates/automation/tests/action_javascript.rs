//! Run-a-JavaScript actions end to end through the tool table: set on links, bookmarks, field
//! triggers and page open/close; run through `js_run_action` in the sandbox; hostile scripts
//! and arguments.

use pdfcraft_automation::{Automation, Content, ToolError};
use serde_json::{Value, json};

fn setup(name: &str) -> (Automation, u64, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("pdfcraft-actjs-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let blank = pdfcraft_engine::Session::new().create_blank(300.0, 300.0, 3).unwrap();
    std::fs::write(dir.join("a.pdf"), blank.as_slice()).unwrap();
    let mut a = Automation::new().with_root(&dir).unwrap().with_clock(|| 1_700_000_000);
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "form_add_field", json!({ "doc": doc, "page": 1, "type": "text", "rect": [20, 20, 220, 42], "name": "Seen" }));
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

fn seen(a: &mut Automation, doc: u64) -> Value {
    ok(a, "form_fields", json!({ "doc": doc }))["fields"][0]["value"].clone()
}

const RECORD: &str = "getField('Seen').value = event.type + ' ' + event.name + ' ' + this.pageNum;";

#[test]
fn javascript_actions_on_links_bookmarks_fields_and_pages() {
    let (mut a, doc, dir) = setup("all");
    // Links.
    ok(&mut a, "link_add", json!({ "doc": doc, "page": 2, "rect": [20, 20, 120, 50], "javascript": RECORD }));
    let links = ok(&mut a, "link_list", json!({ "doc": doc }));
    assert_eq!(links["links"][0]["javascript"], RECORD, "{links}");
    let r = ok(&mut a, "js_run_action", json!({ "doc": doc, "link": { "page": 2, "index": 1 } }));
    assert!(r["error"].is_null(), "{r}");
    assert_eq!(seen(&mut a, doc), "Link Mouse Up 1");
    // A go-to link turned into a script, and back.
    ok(&mut a, "link_add", json!({ "doc": doc, "page": 1, "rect": [20, 60, 120, 90], "to_page": 3 }));
    ok(&mut a, "link_edit", json!({ "doc": doc, "page": 1, "index": 2, "javascript": "app.alert('hi')" }));
    let r = ok(&mut a, "js_run_action", json!({ "doc": doc, "link": { "page": 1, "index": 2 } }));
    assert_eq!(r["alerts"], json!(["hi"]), "{r}");

    // Bookmarks.
    ok(&mut a, "bookmark_add", json!({ "doc": doc, "title": "Script", "page": 1 }));
    ok(&mut a, "bookmark_set_action", json!({ "doc": doc, "path": [1], "javascript": RECORD }));
    let list = ok(&mut a, "bookmark_list", json!({ "doc": doc }));
    assert!(list.to_string().contains("\"type\":\"javascript\""), "{list}");
    ok(&mut a, "js_run_action", json!({ "doc": doc, "bookmark": [1] }));
    assert_eq!(seen(&mut a, doc), "Bookmark Mouse Up 0");

    // Field triggers (Field Properties ▸ Actions).
    ok(&mut a, "form_add_field", json!({ "doc": doc, "page": 1, "type": "button", "rect": [20, 100, 120, 130], "name": "Go" }));
    ok(
        &mut a,
        "form_set_actions",
        json!({ "doc": doc, "field": "Go", "actions": [{ "trigger": "mouse_enter", "javascript": "getField('Seen').value = event.name;" }] }),
    );
    ok(&mut a, "form_run_trigger", json!({ "doc": doc, "field": "Go", "trigger": "mouse_enter" }));
    assert_eq!(seen(&mut a, doc), "Mouse Enter");

    // Page open and close.
    ok(&mut a, "js_set_page_action", json!({ "doc": doc, "page": 3, "trigger": "page_open", "script": RECORD }));
    ok(
        &mut a,
        "js_set_page_action",
        json!({ "doc": doc, "page": 3, "trigger": "page_close", "script": "console.println('closing ' + this.pageNum)" }),
    );
    let one = ok(&mut a, "js_page_actions", json!({ "doc": doc, "page": 3 }));
    assert_eq!(one["actions"][0]["script"], RECORD, "{one}");
    let all = ok(&mut a, "js_page_actions", json!({ "doc": doc }));
    assert_eq!(all["actions"].as_array().unwrap().len(), 2, "{all}");
    ok(&mut a, "js_run_action", json!({ "doc": doc, "page": 3, "trigger": "page_open" }));
    assert_eq!(seen(&mut a, doc), "Page Open 2");
    let r = ok(&mut a, "js_run_action", json!({ "doc": doc, "page": 3, "trigger": "page_close" }));
    assert_eq!(r["console"], json!(["closing 2"]), "{r}");
    // Undoable, and saved.
    ok(&mut a, "js_set_page_action", json!({ "doc": doc, "page": 3, "trigger": "page_close" }));
    assert_eq!(ok(&mut a, "js_page_actions", json!({ "doc": doc }))["actions"].as_array().unwrap().len(), 1);
    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    assert_eq!(ok(&mut a, "js_page_actions", json!({ "doc": doc }))["actions"].as_array().unwrap().len(), 2);
    ok(&mut a, "doc_save", json!({ "doc": doc, "path": "out.pdf" }));
    let again = ok(&mut a, "doc_open", json!({ "path": "out.pdf" }))["doc"].as_u64().unwrap();
    assert_eq!(ok(&mut a, "js_page_actions", json!({ "doc": again }))["actions"].as_array().unwrap().len(), 2);
    let links = ok(&mut a, "link_list", json!({ "doc": again }));
    assert!(links["links"].as_array().unwrap().iter().any(|l| l["page"] == 2 && l["javascript"] == RECORD), "{links}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn hostile_scripts_and_arguments_are_contained() {
    let (mut a, doc, dir) = setup("hostile");
    ok(&mut a, "link_add", json!({ "doc": doc, "page": 2, "rect": [20, 60, 120, 90], "javascript": "1" }));
    for script in ["while (true) {}", "function f() { return f(); } f();", "throw new Error('boom')"] {
        ok(&mut a, "link_edit", json!({ "doc": doc, "page": 2, "index": 1, "javascript": script }));
        let r = ok(&mut a, "js_run_action", json!({ "doc": doc, "link": { "page": 2, "index": 1 } }));
        assert!(r["error"].is_string(), "{script}: {r}");
    }
    // A dialog stops the script and comes back as a question; the answer finishes it.
    ok(
        &mut a,
        "link_edit",
        json!({ "doc": doc, "page": 2, "index": 1, "javascript": "if (app.alert('Sure?', 2, 2) == 4) getField('Seen').value = 'yes';" }),
    );
    let q = ok(&mut a, "js_run_action", json!({ "doc": doc, "link": { "page": 2, "index": 1 } }));
    assert_eq!(q["question"]["kind"], "alert", "{q}");
    ok(&mut a, "js_run_action", json!({ "doc": doc, "link": { "page": 2, "index": 1 }, "answers": ["yes"] }));
    assert_eq!(seen(&mut a, doc), "yes");

    let bad = |a: &mut Automation, tool: &str, args: Value| {
        let r = a.call(tool, &args);
        assert!(matches!(r, Err(ToolError::InvalidArgs(_)) | Err(ToolError::Failed(_))), "{tool} {args}: {r:?}");
    };
    // Over-long and blank scripts, and mixed actions.
    let huge = "x".repeat(pdfcraft_engine::MAX_ACTION_SCRIPT_BYTES + 1);
    bad(&mut a, "link_edit", json!({ "doc": doc, "page": 2, "index": 1, "javascript": huge }));
    bad(&mut a, "js_set_page_action", json!({ "doc": doc, "page": 1, "trigger": "page_open", "script": huge }));
    bad(&mut a, "link_add", json!({ "doc": doc, "page": 1, "rect": [20, 60, 120, 90], "javascript": "  " }));
    bad(&mut a, "link_add", json!({ "doc": doc, "page": 1, "rect": [20, 60, 120, 90], "javascript": "1", "url": "https://example.org" }));
    bad(&mut a, "bookmark_set_action", json!({ "doc": doc, "path": [1], "javascript": "1" }));
    // Nothing to run, bad numbers, no or several sources, unknown triggers.
    bad(&mut a, "js_run_action", json!({ "doc": doc }));
    bad(&mut a, "js_run_action", json!({ "doc": doc, "link": { "page": 2, "index": 1 }, "bookmark": [1] }));
    bad(&mut a, "js_run_action", json!({ "doc": doc, "link": { "page": 0, "index": -3 } }));
    bad(&mut a, "js_run_action", json!({ "doc": doc, "link": { "page": 18446744073709551615u64, "index": 1 } }));
    bad(&mut a, "js_run_action", json!({ "doc": doc, "link": "1" }));
    bad(&mut a, "js_run_action", json!({ "doc": doc, "bookmark": [99, 1] }));
    bad(&mut a, "js_run_action", json!({ "doc": doc, "page": 2, "trigger": "page_open" }));
    bad(&mut a, "js_run_action", json!({ "doc": doc, "page": 99, "trigger": "page_open" }));
    bad(&mut a, "js_run_action", json!({ "doc": doc, "page": 1, "trigger": "on_open" }));
    bad(&mut a, "js_set_page_action", json!({ "doc": doc, "page": 0, "trigger": "page_open", "script": "1" }));
    // With JavaScript off, nothing runs.
    ok(&mut a, "js_enabled", json!({ "enabled": false }));
    bad(&mut a, "js_run_action", json!({ "doc": doc, "link": { "page": 2, "index": 1 } }));
    let _ = std::fs::remove_dir_all(dir);
}
