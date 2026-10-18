//! Custom commands end to end: create, list, edit, delete, import, export and run them through
//! the tool table, on synthetic PDFs, under a root.

use std::path::{Path, PathBuf};

use pdfcraft_automation::{Automation, Content, ToolError};
use serde_json::{Value, json};

/// A fresh root holding `a.pdf` (2 pages) and `b.pdf` (1 page).
fn workdir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pdfcraft-custom-commands-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let s = pdfcraft_engine::Session::new();
    std::fs::write(dir.join("a.pdf"), s.create_blank(200.0, 300.0, 2).unwrap().as_slice()).unwrap();
    std::fs::write(dir.join("b.pdf"), s.create_blank(200.0, 300.0, 1).unwrap().as_slice()).unwrap();
    dir
}

fn auto(dir: &Path) -> Automation {
    Automation::new().with_root(dir).unwrap()
}

fn ok(a: &mut Automation, tool: &str, args: Value) -> Value {
    match a.call(tool, &args) {
        Ok(c) => c
            .into_iter()
            .find_map(|c| match c {
                Content::Json(v) => Some(v),
                Content::Png { .. } => None,
            })
            .unwrap(),
        Err(e) => panic!("{tool} {args}: {e}"),
    }
}

fn err(a: &mut Automation, tool: &str, args: Value) -> String {
    match a.call(tool, &args) {
        Ok(_) => panic!("{tool} {args}: expected an error"),
        Err(e) => e.to_string(),
    }
}

fn title(a: &mut Automation, path: &str) -> Value {
    let doc = ok(a, "doc_open", json!({ "path": path }))["doc"].as_u64().unwrap();
    let info = ok(a, "doc_info", json!({ "doc": doc }));
    ok(a, "doc_close", json!({ "doc": doc }));
    info["title"].clone()
}

#[test]
fn create_list_get_edit_delete() {
    let dir = workdir("crud");
    let mut a = auto(&dir);
    let empty = ok(&mut a, "custom_command_list", json!({}));
    assert_eq!(empty["commands"], json!([]));
    let tools: Vec<&str> = empty["tools"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    for t in ["doc_watermark", "ocr_recognize", "doc_optimize", "doc_remove_hidden", "doc_set_info"] {
        assert!(tools.contains(&t), "{t} in {tools:?}");
    }
    assert!(!tools.contains(&"doc_save") && !tools.contains(&"custom_command_run") && !tools.contains(&"command_run"));

    let created = ok(
        &mut a,
        "custom_command_create",
        json!({ "id": "draft-mark", "label": "Mark as draft", "tool": "doc_watermark", "params": { "text": "DRAFT", "opacity": 0.3 } }),
    );
    assert_eq!(created["created"]["prompt_user"], false);
    let stored: Value = serde_json::from_slice(&std::fs::read(dir.join("custom-commands/draft-mark.json")).unwrap()).unwrap();
    assert_eq!(stored["format"], "pdfcraft-custom-command");
    assert_eq!(stored["params"]["text"], "DRAFT");

    // Same id again, and a broken file next to it.
    assert!(
        err(&mut a, "custom_command_create", json!({ "id": "draft-mark", "label": "x", "tool": "doc_watermark", "params": { "text": "X" } }))
            .contains("already exists")
    );
    std::fs::write(dir.join("custom-commands/broken.json"), b"{ not json").unwrap();
    let list = ok(&mut a, "custom_command_list", json!({}));
    assert_eq!(list["commands"].as_array().unwrap().len(), 1);
    assert_eq!(list["skipped"][0]["file"], "broken.json");

    let got = ok(&mut a, "custom_command_get", json!({ "id": "draft-mark" }));
    assert_eq!((got["tool"].as_str(), got["needs"].clone(), got["options"].clone()), (Some("doc_watermark"), json!([]), Value::Null));

    let edited = ok(&mut a, "custom_command_edit", json!({ "id": "draft-mark", "label": "Draft stamp", "prompt_user": true }));
    assert_eq!((edited["changed"].as_bool(), edited["edited"]["label"].as_str()), (Some(true), Some("Draft stamp")));
    let got = ok(&mut a, "custom_command_get", json!({ "id": "draft-mark" }));
    assert!(got["options"]["properties"]["opacity"].is_object());
    assert!(got["options"]["properties"].get("doc").is_none());
    let same = ok(&mut a, "custom_command_edit", json!({ "id": "draft-mark", "label": "Draft stamp" }));
    assert_eq!(same["changed"], false);
    // An edit that doesn't validate leaves the stored command as it was.
    assert!(err(&mut a, "custom_command_edit", json!({ "id": "draft-mark", "params": { "opacity": 7 } })).contains("at most 1"));
    assert_eq!(ok(&mut a, "custom_command_get", json!({ "id": "draft-mark" }))["params"]["text"], "DRAFT");

    ok(&mut a, "custom_command_delete", json!({ "id": "draft-mark" }));
    assert!(!dir.join("custom-commands/draft-mark.json").exists());
    assert!(err(&mut a, "custom_command_delete", json!({ "id": "draft-mark" })).contains("no custom command"));
    assert!(err(&mut a, "custom_command_get", json!({ "id": "draft-mark" })).contains("no custom command"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn params_are_validated_against_the_tool_schema() {
    let dir = workdir("validate");
    let mut a = auto(&dir);
    let create = |a: &mut Automation, tool: &str, params: Value, prompt: bool| {
        a.call("custom_command_create", &json!({ "id": "c", "label": "C", "tool": tool, "params": params, "prompt_user": prompt }))
    };
    let invalid = |r: Result<Vec<Content>, ToolError>| match r {
        Err(ToolError::InvalidArgs(m)) => m,
        other => panic!("expected invalid arguments, got {other:?}"),
    };
    assert!(invalid(create(&mut a, "doc_watermark", json!({ "txet": "x" }), false)).contains("unknown option \"txet\""));
    assert!(invalid(create(&mut a, "doc_watermark", json!({ "text": 3 }), false)).contains("type string"));
    assert!(invalid(create(&mut a, "doc_watermark", json!({ "text": "" }), false)).contains("at least 1"));
    assert!(invalid(create(&mut a, "doc_watermark", json!({ "doc": 1 }), false)).contains("doc"));
    assert!(invalid(create(&mut a, "doc_watermark", json!({ "pages": [0] }), false)).contains("at least 1"));
    assert!(invalid(create(&mut a, "doc_remove_hidden", json!({ "categories": ["everything"] }), false)).contains("one of"));
    assert!(invalid(create(&mut a, "doc_save", json!({}), false)).contains("can't be a custom command"));
    assert!(invalid(create(&mut a, "custom_command_run", json!({}), false)).contains("can't be a custom command"));
    assert!(invalid(create(&mut a, "no_such_tool", json!({}), false)).contains("unknown tool"));
    // A required option (doc_set_info's key and value) must be preset, unless the user is asked.
    assert!(invalid(create(&mut a, "doc_set_info", json!({ "key": "Title" }), false)).contains("prompt_user"));
    ok(
        &mut a,
        "custom_command_create",
        json!({ "id": "c", "label": "C", "tool": "doc_set_info", "params": { "key": "Title" }, "prompt_user": true }),
    );
    assert_eq!(ok(&mut a, "custom_command_get", json!({ "id": "c" }))["needs"], json!(["value"]));
    // Hostile ids and labels.
    for id in ["../x", "A", "", "a/b", "x.json"] {
        assert!(a.call("custom_command_create", &json!({ "id": id, "label": "x", "tool": "doc_flatten" })).is_err(), "{id}");
    }
    assert!(a.call("custom_command_create", &json!({ "id": "x", "label": "a\u{0}b", "tool": "doc_flatten" })).is_err());
    assert!(a.call("custom_command_create", &json!({ "id": "x", "label": "x", "tool": "doc_flatten", "params": [] })).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn run_on_a_document_and_prompt_for_options() {
    let dir = workdir("run-doc");
    let mut a = auto(&dir);
    ok(
        &mut a,
        "custom_command_create",
        json!({ "id": "title", "label": "Set title", "tool": "doc_set_info", "params": { "key": "Title", "value": "Report {stem}" } }),
    );
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    let r = ok(&mut a, "custom_command_run", json!({ "id": "title", "doc": doc }));
    assert_eq!((r["tool"].as_str(), r["params"]["value"].as_str()), (Some("doc_set_info"), Some("Report a")));
    assert_eq!(r["result"]["dirty"], true, "{r}");
    assert_eq!(ok(&mut a, "doc_info", json!({ "doc": doc }))["title"], "Report a");
    // One undo step.
    ok(&mut a, "edit_undo", json!({ "doc": doc }));
    assert_ne!(ok(&mut a, "doc_info", json!({ "doc": doc }))["title"], "Report a");
    // Without prompt_user the presets are fixed.
    assert!(err(&mut a, "custom_command_run", json!({ "id": "title", "doc": doc, "params": { "value": "Other" } })).contains("prompt_user"));

    ok(
        &mut a,
        "custom_command_create",
        json!({ "id": "ask", "label": "Ask for a title", "tool": "doc_set_info", "params": { "key": "Title" }, "prompt_user": true }),
    );
    assert!(err(&mut a, "custom_command_run", json!({ "id": "ask", "doc": doc })).contains("value"));
    assert!(err(&mut a, "custom_command_run", json!({ "id": "ask", "doc": doc, "params": { "value": 5 } })).contains("type string"));
    assert!(err(&mut a, "custom_command_run", json!({ "id": "ask", "doc": doc, "params": { "doc": 9 } })).contains("doc"));
    ok(&mut a, "custom_command_run", json!({ "id": "ask", "doc": doc, "params": { "value": "Asked" } }));
    assert_eq!(ok(&mut a, "doc_info", json!({ "doc": doc }))["title"], "Asked");
    // doc or paths, not both or neither; an unknown document.
    assert!(err(&mut a, "custom_command_run", json!({ "id": "title" })).contains("doc"));
    assert!(err(&mut a, "custom_command_run", json!({ "id": "title", "doc": doc, "paths": ["a.pdf"], "folder": "out" })).contains("not both"));
    assert!(err(&mut a, "custom_command_run", json!({ "id": "title", "doc": 999 })).contains("no open document"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn run_on_files_under_the_root() {
    let dir = workdir("run-files");
    let mut a = auto(&dir);
    ok(
        &mut a,
        "custom_command_create",
        json!({ "id": "title", "label": "Set title", "tool": "doc_set_info", "params": { "key": "Title", "value": "Batch {file}" } }),
    );
    let r = ok(&mut a, "custom_command_run", json!({ "id": "title", "paths": ["a.pdf", "b.pdf", "missing.pdf", "../outside.pdf"], "folder": "out" }));
    assert_eq!((r["completed"].as_u64(), r["failed"].as_u64()), (Some(2), Some(2)), "{r}");
    assert!(r["files"][2]["error"].is_string());
    assert!(r["files"][3]["error"].as_str().unwrap().contains("outside the allowed directory"), "{r}");
    assert_eq!(title(&mut a, "out/a.pdf"), "Batch a.pdf");
    assert_eq!(title(&mut a, "out/b.pdf"), "Batch b.pdf");
    // Inputs untouched, nothing left open.
    assert_ne!(title(&mut a, "a.pdf"), "Batch a.pdf");
    assert_eq!(ok(&mut a, "doc_list", json!({}))["documents"], json!([]));
    // Never replaces its input, and the same name twice is refused for the second.
    let r = ok(&mut a, "custom_command_run", json!({ "id": "title", "paths": ["a.pdf"], "folder": "." }));
    assert!(r["files"][0]["error"].as_str().unwrap().contains("replace the input"), "{r}");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::copy(dir.join("a.pdf"), dir.join("sub/a.pdf")).unwrap();
    let r = ok(&mut a, "custom_command_run", json!({ "id": "title", "paths": ["a.pdf", "sub/a.pdf"], "folder": "out2" }));
    assert!(r["files"][1]["error"].as_str().unwrap().contains("already writes"), "{r}");
    // stop_on_error stops at the first failure.
    let r = ok(&mut a, "custom_command_run", json!({ "id": "title", "paths": ["missing.pdf", "a.pdf"], "folder": "out3", "stop_on_error": true }));
    assert_eq!(r["files"].as_array().unwrap().len(), 1);
    // The output folder and the library are confined too.
    assert!(err(&mut a, "custom_command_run", json!({ "id": "title", "paths": ["a.pdf"], "folder": "../escape" })).contains("outside"));
    assert!(err(&mut a, "custom_command_list", json!({ "library": "../lib" })).contains("outside"));
    assert!(!dir.parent().unwrap().join("escape").exists());
    // A tool that doesn't change the document leaves no output.
    ok(&mut a, "custom_command_create", json!({ "id": "noop", "label": "Flatten", "tool": "doc_hidden_info" }));
    let r = ok(&mut a, "custom_command_run", json!({ "id": "noop", "paths": ["b.pdf"], "folder": "out4" }));
    assert_eq!(r["files"][0]["unchanged"], true, "{r}");
    assert!(!dir.join("out4/b.pdf").exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn export_and_import() {
    let dir = workdir("io");
    let mut a = auto(&dir);
    ok(&mut a, "custom_command_create", json!({ "id": "one", "label": "One", "tool": "doc_watermark", "params": { "text": "ONE" } }));
    ok(
        &mut a,
        "custom_command_create",
        json!({ "id": "two", "label": "Two", "tool": "doc_remove_hidden", "params": { "categories": ["metadata"] }, "description": "Strip metadata." }),
    );
    let r = ok(&mut a, "custom_command_export", json!({ "path": "shared/cmds.json" }));
    assert_eq!(r["exported"], json!(["one", "two"]));
    let r = ok(&mut a, "custom_command_export", json!({ "path": "shared/one.json", "ids": ["one"] }));
    assert_eq!(r["exported"], json!(["one"]));
    assert!(err(&mut a, "custom_command_export", json!({ "path": "x.json", "ids": ["nope"] })).contains("no custom command"));
    assert!(err(&mut a, "custom_command_export", json!({ "path": "../x.json" })).contains("outside"));

    // Into another library: everything arrives; again only with replace.
    let r = ok(&mut a, "custom_command_import", json!({ "path": "shared/cmds.json", "library": "lib2" }));
    assert_eq!(r["imported"], json!(["one", "two"]));
    let list = ok(&mut a, "custom_command_list", json!({ "library": "lib2" }));
    assert_eq!(list["commands"][1]["description"], "Strip metadata.");
    assert!(err(&mut a, "custom_command_import", json!({ "path": "shared/cmds.json", "library": "lib2" })).contains("replace"));
    let r = ok(&mut a, "custom_command_import", json!({ "path": "shared/cmds.json", "library": "lib2", "replace": true }));
    assert_eq!(r["replaced"], json!(["one", "two"]));

    // One command file on its own imports too; a bad command anywhere imports nothing.
    std::fs::write(
        dir.join("single.json"),
        serde_json::to_vec(&json!({ "id": "solo", "label": "Solo", "tool": "doc_flatten", "params": {} })).unwrap(),
    )
    .unwrap();
    assert_eq!(ok(&mut a, "custom_command_import", json!({ "path": "single.json", "library": "lib3" }))["imported"], json!(["solo"]));
    let bad = json!({ "format": "pdfcraft-custom-commands", "version": 1, "commands": [
        { "id": "fine", "label": "Fine", "tool": "doc_flatten" },
        { "id": "evil", "label": "Evil", "tool": "doc_watermark", "params": { "opacity": "high" } },
    ] });
    std::fs::write(dir.join("bad.json"), serde_json::to_vec(&bad).unwrap()).unwrap();
    assert!(err(&mut a, "custom_command_import", json!({ "path": "bad.json", "library": "lib4" })).contains("command 2"));
    assert!(!dir.join("lib4/fine.json").exists());
    for hostile in [
        json!({ "commands": "no" }),
        json!({ "commands": [{ "id": "../../x", "label": "x", "tool": "doc_flatten" }] }),
        json!({ "commands": [{ "id": "d", "label": "x", "tool": "doc_flatten" }, { "id": "d", "label": "y", "tool": "doc_flatten" }] }),
        json!({ "commands": [{ "id": "u", "label": "x", "tool": "doc_flatten", "surprise": true }] }),
        json!({ "format": "pdfcraft-custom-commands", "version": 99, "commands": [] }),
        json!([1, 2, 3]),
    ] {
        std::fs::write(dir.join("hostile.json"), serde_json::to_vec(&hostile).unwrap()).unwrap();
        assert!(a.call("custom_command_import", &json!({ "path": "hostile.json", "library": "lib5" })).is_err(), "{hostile}");
    }
    std::fs::write(dir.join("garbage.json"), [0xffu8, 0, 1, 2]).unwrap();
    assert!(err(&mut a, "custom_command_import", json!({ "path": "garbage.json" })).contains("not JSON"));
    assert!(!dir.join("lib5").join("x.json").exists());

    // An imported command runs like any other.
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    let r = ok(&mut a, "custom_command_run", json!({ "id": "one", "doc": doc, "library": "lib2" }));
    assert_eq!(r["result"]["dirty"], true, "{r}");
    let _ = std::fs::remove_dir_all(dir);
}
