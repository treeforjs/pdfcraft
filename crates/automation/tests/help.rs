//! `help_shortcuts` and `help_guide` end to end through the tool table, with hostile input.

use pdfcraft_automation::{Automation, Content};
use serde_json::{Value, json};

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
        Ok(_) => panic!("{tool} {args} should fail"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn shortcuts_list_the_registry_bindings_and_search_them() {
    let mut a = Automation::new();
    let all = ok(&mut a, "help_shortcuts", json!({}));
    let rows = all["shortcuts"].as_array().unwrap();
    assert_eq!(all["count"], rows.len());
    let save_as = rows.iter().find(|r| r["command"] == "file.save_as").unwrap();
    assert_eq!(save_as["mac"], "⇧⌘S");
    assert_eq!(save_as["other"], "Ctrl+Shift+S");
    assert_eq!(save_as["section"], "File");
    assert!(rows.iter().any(|r| r["command"].is_null() && r["action"] == "Previous view"), "view keys are listed too");

    let found = ok(&mut a, "help_shortcuts", json!({ "query": "save AS", "platform": "other" }));
    assert!(found["shortcuts"].as_array().unwrap().iter().any(|r| r["command"] == "file.save_as" && r.get("mac").is_none()));
    let none = ok(&mut a, "help_shortcuts", json!({ "query": "zzzz-nothing" }));
    assert_eq!(none["count"], 0);
    assert!(err(&mut a, "help_shortcuts", json!({ "platform": "amiga" })).contains("mac, other or both"));
    // The tool is what command_run runs for the menu command.
    let via = ok(&mut a, "command_run", json!({ "id": "help.shortcuts", "params": { "query": "undo" } }));
    assert!(via.to_string().contains("edit.undo"), "{via}");
}

#[test]
fn guide_lists_pages_returns_one_and_searches() {
    let mut a = Automation::new();
    let list = ok(&mut a, "help_guide", json!({}));
    let pages = list["pages"].as_array().unwrap();
    assert!(pages.len() >= 15);
    assert!(pages.iter().any(|p| p["page"] == "signing" && p["title"] == "Digital signatures"));
    let page = ok(&mut a, "help_guide", json!({ "page": "keyboard.md" }));
    assert_eq!(page["page"], "keyboard");
    assert!(page["markdown"].as_str().unwrap().contains("| Save as | ⇧⌘S | Ctrl+Shift+S |"));
    let hits = ok(&mut a, "help_guide", json!({ "query": "redaction", "limit": 3 }));
    assert!(hits["count"].as_u64().unwrap() <= 3 && hits["count"].as_u64().unwrap() > 0);
    let unknown = err(&mut a, "help_guide", json!({ "page": "nope" }));
    assert!(unknown.contains("the pages are") && unknown.contains("viewing"), "{unknown}");
    assert!(err(&mut a, "help_guide", json!({ "page": "viewing", "query": "zoom" })).contains("not both"));
}

#[test]
fn hostile_help_arguments_are_refused_or_bounded() {
    let mut a = Automation::new();
    let huge = "\u{202e}🙂\0".repeat(100_000);
    for args in [json!({ "query": huge }), json!({ "query": "(.*)[]{}\\^$|?+ %s%n" }), json!({ "query": "\u{0}\u{1}\n" })] {
        let _ = ok(&mut a, "help_shortcuts", args.clone());
        let _ = a.call("help_guide", &args);
    }
    for args in [
        json!({ "query": 5 }),
        json!({ "platform": 3 }),
        json!({ "page": ["viewing"] }),
        json!({ "page": "../../../etc/passwd" }),
        json!({ "page": huge }),
        json!({ "query": "zoom", "limit": 0 }),
        json!({ "query": "zoom", "limit": -1 }),
        json!({ "query": "zoom", "limit": 1_000_000_000_000i64 }),
    ] {
        let tool = if args.get("platform").is_some() { "help_shortcuts" } else { "help_guide" };
        let e = err(&mut a, tool, args.clone());
        assert!(e.len() < 2000, "errors stay short: {}", e.len());
    }
}
