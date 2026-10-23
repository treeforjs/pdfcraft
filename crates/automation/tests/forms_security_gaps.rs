//! Keystroke checks while typing, and the user's own redaction code sets, through the same tools
//! used by the CLI and MCP. All values are synthetic.

use pdfcraft_automation::{Automation, Content, ToolError};
use serde_json::{Value, json};

fn tool(a: &mut Automation, name: &str, args: Value) -> Value {
    match a.call(name, &args).unwrap_or_else(|e| panic!("{name} {args}: {e:?}")).remove(0) {
        Content::Json(v) => v,
        other => panic!("expected JSON: {other:?}"),
    }
}

fn setup(test: &str) -> (Automation, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("pdfcraft-forms-security-{test}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    (Automation::new().with_root(&dir).unwrap(), dir)
}

#[test]
fn keystrokes_are_checked_by_format_and_script_while_typing() {
    let (mut a, dir) = setup("keystroke");
    let doc = tool(&mut a, "doc_create", json!({ "from": "blank" }))["doc"].as_u64().unwrap();
    for (name, y) in [("Amount", 20), ("Code", 60), ("Plain", 100)] {
        tool(&mut a, "form_add_field", json!({ "doc": doc, "page": 1, "type": "text", "rect": [20, y, 180, y + 22], "name": name }));
    }
    tool(&mut a, "form_set_props", json!({ "doc": doc, "field": "Amount", "format": { "type": "number", "decimals": 2 } }));
    let r = tool(&mut a, "form_keystroke", json!({ "doc": doc, "field": "Amount", "value": "12", "change": "3" }));
    assert_eq!((r["accepted"].as_bool(), r["result"].as_str(), r["checked_by"].as_str()), (Some(true), Some("123"), Some("format")), "{r}");
    let r = tool(&mut a, "form_keystroke", json!({ "doc": doc, "field": "Amount", "value": "12", "change": "x" }));
    assert_eq!((r["accepted"].as_bool(), r["result"].as_str()), (Some(false), Some("12")), "letters are refused in a number: {r}");
    // A custom Keystroke script upper-cases what is typed and refuses digits.
    tool(&mut a, "js_enabled", json!({ "enabled": true }));
    let script = "if (/[0-9]/.test(event.change)) event.rc = false; else event.change = event.change.toUpperCase();";
    tool(&mut a, "form_set_script", json!({ "doc": doc, "field": "Code", "event": "keystroke", "script": script }));
    let r = tool(&mut a, "form_keystroke", json!({ "doc": doc, "field": "Code", "value": "AB", "change": "c", "sel": [1, 2] }));
    assert_eq!(
        (r["accepted"].as_bool(), r["change"].as_str(), r["result"].as_str(), r["checked_by"].as_str()),
        (Some(true), Some("C"), Some("AC"), Some("script")),
        "{r}"
    );
    let r = tool(&mut a, "form_keystroke", json!({ "doc": doc, "field": "Code", "value": "AB", "change": "7" }));
    assert_eq!(r["accepted"], false, "{r}");
    let r = tool(&mut a, "form_keystroke", json!({ "doc": doc, "field": "Plain", "change": "anything" }));
    assert_eq!((r["accepted"].as_bool(), r["checked_by"].as_str()), (Some(true), Some("none")));
    // Hostile: positions past the end are clamped, a huge paste is refused, bad shapes fail.
    let r = tool(&mut a, "form_keystroke", json!({ "doc": doc, "field": "Code", "value": "AB", "change": "z", "sel": [99, 3] }));
    assert_eq!(r["result"], "ABZ", "{r}");
    let r = tool(&mut a, "form_keystroke", json!({ "doc": doc, "field": "Plain", "change": "x".repeat(70_000) }));
    assert_eq!(r["accepted"], false, "{r}");
    for bad in [
        json!({ "doc": doc, "field": "Nope", "change": "1" }),
        json!({ "doc": doc, "field": "Amount", "sel": [1] }),
        json!({ "doc": doc, "field": "Amount", "sel": "1,2" }),
    ] {
        assert!(a.call("form_keystroke", &bad).is_err(), "{bad}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn custom_code_sets_are_saved_cited_exported_imported_and_deleted() {
    let (mut a, dir) = setup("code-sets");
    let r = tool(&mut a, "redact_code_set_save", json!({ "name": "Contract", "codes": [{ "code": "C-1", "description": "Pricing" }, "C-2"] }));
    assert_eq!((r["codes"].as_u64(), r["replaced"].as_bool()), (Some(2), Some(false)));
    let sets = tool(&mut a, "redact_codes", json!({}));
    let mine = sets["code_sets"].as_array().unwrap().iter().find(|s| s["custom"] == true).unwrap().clone();
    assert_eq!((mine["name"].as_str(), mine["codes"][0]["description"].as_str()), (Some("Contract"), Some("Pricing")));
    assert_eq!(tool(&mut a, "redact_codes", json!({ "code_set": "contract" }))["code_sets"].as_array().unwrap().len(), 1);

    std::fs::write(dir.join("memo.txt"), "Unit price 4200 per seat").unwrap();
    let doc = tool(&mut a, "doc_create", json!({ "from": "text", "path": "memo.txt" }))["doc"].as_u64().unwrap();
    let r = tool(&mut a, "redact_mark", json!({ "doc": doc, "find": "4200", "code_set": "Contract", "codes": ["c-2", "C-1"] }));
    assert_eq!(r["overlay"], "C-2, C-1", "codes are written in the set's form: {r}");
    assert!(matches!(
        a.call("redact_mark", &json!({ "doc": doc, "find": "4200", "code_set": "Contract", "codes": ["C-9"] })),
        Err(ToolError::InvalidArgs(_))
    ));

    let out = tool(&mut a, "redact_code_sets_file", json!({ "mode": "export", "path": "sets.json" }));
    assert_eq!(out["code_sets"], 1);
    tool(&mut a, "redact_code_set_delete", json!({ "name": "CONTRACT" }));
    assert!(a.call("redact_code_set_delete", &json!({ "name": "Contract" })).is_err(), "already gone");
    let r = tool(&mut a, "redact_code_sets_file", json!({ "mode": "import", "path": "sets.json" }));
    assert_eq!(r["imported"], json!(["Contract"]));

    // Hostile: built-in names, empty or oversized sets, broken files, unknown modes.
    std::fs::write(dir.join("bad.json"), b"{\"code_sets\": [{\"name\": \"x\", \"codes\": [7]}]}").unwrap();
    std::fs::write(dir.join("huge.json"), vec![b' '; (1 << 20) + 1]).unwrap();
    let many: Vec<Value> = (0..501).map(|i| json!(format!("K{i}"))).collect();
    for (name, args) in [
        ("redact_code_set_save", json!({ "name": "us-foia", "codes": ["A"] })),
        ("redact_code_set_save", json!({ "name": "U.S. FOIA", "codes": ["A"] })),
        ("redact_code_set_save", json!({ "name": "", "codes": ["A"] })),
        ("redact_code_set_save", json!({ "name": "Empty", "codes": [] })),
        ("redact_code_set_save", json!({ "name": "Lines", "codes": ["a\nb"] })),
        ("redact_code_set_save", json!({ "name": "Many", "codes": many })),
        ("redact_code_set_save", json!({ "name": "Shape", "codes": "A" })),
        ("redact_code_sets_file", json!({ "mode": "import", "path": "bad.json" })),
        ("redact_code_sets_file", json!({ "mode": "import", "path": "huge.json" })),
        ("redact_code_sets_file", json!({ "mode": "merge", "path": "sets.json" })),
    ] {
        assert!(a.call(name, &args).is_err(), "{name} {args}");
    }
    assert_eq!(a.session().redaction_code_sets().len(), 1, "refused calls change nothing");
    let _ = std::fs::remove_dir_all(dir);
}
