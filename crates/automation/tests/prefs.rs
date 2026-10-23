//! Preferences end to end through the tool table: `prefs_get` (values and schema) and
//! `prefs_set` (one, several, reset), their effect on the other tools (comment authors, colour
//! settings, JavaScript, internet access) and hostile values.

use pdfcraft_automation::{Automation, Content};
use serde_json::{Value, json};

fn pdf() -> Vec<u8> {
    let objs: [&[u8]; 3] = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 400] >>",
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
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
        Ok(_) => panic!("{tool} {args} should fail"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn prefs_read_set_and_reach_the_other_tools() {
    let dir = std::env::temp_dir().join(format!("pdfcraft-prefs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("doc.pdf"), pdf()).unwrap();
    let mut a = Automation::new().with_root(&dir).unwrap();

    // The schema: every key with its type, range and default.
    let all = ok(&mut a, "prefs_get", json!({ "schema": true }));
    let schema = all["schema"].as_array().unwrap();
    assert!(schema.len() >= 20, "{all}");
    let autosave = schema.iter().find(|s| s["key"] == "documents.autosave_minutes").unwrap();
    assert_eq!((autosave["type"].as_str(), autosave["minimum"].as_i64(), autosave["default"].as_i64()), (Some("integer"), Some(0), Some(1)));
    assert_eq!(all["values"]["identity.name"], "");
    let color = ok(&mut a, "prefs_get", json!({ "category": "color" }));
    assert!(color["values"].as_object().unwrap().keys().all(|k| k.starts_with("color.")), "{color}");
    assert_eq!(ok(&mut a, "prefs_get", json!({ "key": "color.intent" }))["value"], "relative_colorimetric");

    // identity.name signs new comments that don't name an author.
    ok(&mut a, "prefs_set", json!({ "key": "identity.name", "value": "Grace Hopper" }));
    let doc = ok(&mut a, "doc_open", json!({ "path": "doc.pdf" }))["doc"].as_u64().unwrap();
    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 1, "type": "note", "at": [20, 20], "contents": "hi" }));
    ok(&mut a, "comment_add", json!({ "doc": doc, "page": 1, "type": "note", "at": [60, 20], "contents": "hi", "author": "Ada" }));
    let list = ok(&mut a, "comment_list", json!({ "doc": doc }));
    let authors: Vec<&str> = list["comments"].as_array().unwrap().iter().filter_map(|c| c["author"].as_str()).collect();
    assert_eq!(authors, ["Grace Hopper", "Ada"], "{list}");

    // Session keys are the same settings the dedicated tools change.
    let set = ok(
        &mut a,
        "prefs_set",
        json!({ "values": { "color.intent": "perceptual", "security.javascript": false, "security.internet_access": "block-all", "documents.recent_files": 400 } }),
    );
    assert_eq!(set["values"]["documents.recent_files"], 50);
    assert_eq!(set["clamped"], json!(["documents.recent_files"]));
    assert_eq!(ok(&mut a, "color_settings_get", json!({}))["settings"]["intent"], "perceptual");
    assert_eq!(ok(&mut a, "js_enabled", json!({}))["enabled"], false);
    assert_eq!(ok(&mut a, "trust_urls_get", json!({}))["mode"], "block-all");
    ok(&mut a, "color_settings_set", json!({ "black_point_compensation": true }));
    assert_eq!(ok(&mut a, "prefs_get", json!({ "key": "color.black_point_compensation" }))["value"], true);

    // All or none: one refused value leaves the others as they were.
    let e = err(&mut a, "prefs_set", json!({ "values": { "identity.email": "a@b.c", "color.rgb": "user:missing" } }));
    assert!(e.contains("color.rgb") || e.contains("user:missing"), "{e}");
    assert_eq!(ok(&mut a, "prefs_get", json!({ "key": "identity.email" }))["value"], "");

    // Reset puts keys back.
    ok(&mut a, "prefs_set", json!({ "reset": ["color.intent", "security.javascript"] }));
    assert_eq!(ok(&mut a, "js_enabled", json!({}))["enabled"], true);
    assert_eq!(ok(&mut a, "prefs_get", json!({ "key": "color.intent" }))["value"], "relative_colorimetric");
    ok(&mut a, "prefs_set", json!({ "reset": true }));
    assert_eq!(ok(&mut a, "prefs_get", json!({ "key": "identity.name" }))["value"], "");
    assert_eq!(ok(&mut a, "trust_urls_get", json!({}))["mode"], "custom");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hostile_pref_arguments_are_refused_or_clamped() {
    let mut a = Automation::new();
    for args in [
        json!({}),
        json!({ "key": "no.such" , "value": 1 }),
        json!({ "key": "identity.name" }),
        json!({ "value": 3 }),
        json!({ "values": [1, 2] }),
        json!({ "reset": "yes" }),
        json!({ "reset": [1] }),
        json!({ "key": "documents.autosave_minutes", "value": "soon" }),
        json!({ "key": "documents.autosave_minutes", "value": null }),
        json!({ "key": "accessibility.reduce_motion", "value": 7 }),
        json!({ "key": "forms.highlight_color", "value": "#XYZ" }),
        json!({ "key": "forms.highlight_color", "value": [1, 2, 3, 4] }),
        json!({ "key": "security.internet_access", "value": "everything" }),
        json!({ "key": "color.gray", "value": "srgb" }),
        json!({ "key": "forms.date_format", "value": "HH:MM" }),
    ] {
        let e = err(&mut a, "prefs_set", args.clone());
        assert!(!e.is_empty(), "{args}");
    }
    assert!(err(&mut a, "prefs_get", json!({ "category": "nope" })).contains("general"));
    assert!(err(&mut a, "prefs_get", json!({ "key": "nope" })).contains("identity.name"));
    let big = ok(&mut a, "prefs_set", json!({ "key": "accessibility.text_scale", "value": 1e308 }));
    assert_eq!(big["values"]["accessibility.text_scale"], 2.0);
    let neg = ok(&mut a, "prefs_set", json!({ "key": "documents.autosave_minutes", "value": -1e12 }));
    assert_eq!(neg["values"]["documents.autosave_minutes"], 0);
    let name = ok(&mut a, "prefs_set", json!({ "key": "identity.name", "value": "\u{1b}[31m".repeat(5000) + "Ada" }));
    let stored = name["values"]["identity.name"].as_str().unwrap();
    assert!(!stored.chars().any(char::is_control) && stored.chars().count() <= 200, "{stored}");
    // Nothing above changed the defaults it refused.
    let values = ok(&mut a, "prefs_get", json!({}))["values"].clone();
    assert_eq!(values["forms.highlight_color"], "#6E8EF5");
    assert_eq!(values["security.internet_access"], "custom");
    assert_eq!(values["color.gray"], "sgray");
}

/// `ui_language_get` / `ui_language_set` read and write `general.language` (the key the desktop
/// app reads), `locale_format` writes numbers, dates and plurals per language, and hostile
/// arguments are refused with what is allowed.
#[test]
fn interface_language_through_tools() {
    let mut a = Automation::new();
    let got = ok(&mut a, "ui_language_get", json!({}));
    assert_eq!(got["preference"], "auto");
    let languages = got["languages"].as_array().unwrap();
    assert_eq!(languages.len(), 15);
    assert!(languages.iter().all(|l| l["direction"] == "ltr"), "no right-to-left language is offered");
    let de = languages.iter().find(|l| l["code"] == "de").unwrap();
    assert_eq!(de["sample_number"], "1.234.567,5");
    assert_eq!(de["sample_date"], "08.10.2026");

    let set = ok(&mut a, "ui_language_set", json!({ "language": "de_AT.UTF-8" }));
    assert_eq!((set["preference"].as_str(), set["language"].as_str(), set["name"].as_str()), (Some("de"), Some("de"), Some("Deutsch")));
    assert_eq!(ok(&mut a, "prefs_get", json!({ "key": "general.language" }))["value"], "de");
    // The interface language in effect is the default for locale_format.
    assert_eq!(ok(&mut a, "locale_format", json!({ "number": 1234.5 }))["number"], "1.234,50");
    ok(&mut a, "prefs_set", json!({ "key": "general.language", "value": "JA" }));
    assert_eq!(ok(&mut a, "ui_language_get", json!({}))["language"], "ja");

    let f = ok(
        &mut a,
        "locale_format",
        json!({ "language": "ru", "number": -0.5, "decimals": 1, "integer": 1234567, "date": "2026-10-08", "pdf_date": "D:20261008140500Z", "count": 22 }),
    );
    assert_eq!(f["number"], "-0,5");
    assert_eq!(f["integer"], "1\u{a0}234\u{a0}567");
    assert_eq!(f["date"], "08.10.2026");
    assert_eq!(f["pdf_date"], "08.10.2026 14:05");
    assert_eq!(f["count"], json!({ "text": "22", "plural_category": "few", "plural_form": 1 }));
    assert_eq!(ok(&mut a, "locale_format", json!({ "language": "te", "integer": 123456789 }))["integer"], "12,34,56,789");

    for (tool, args) in [
        ("ui_language_set", json!({ "language": "klingon" })),
        ("ui_language_set", json!({ "language": "x".repeat(100_000) })),
        ("ui_language_set", json!({ "language": 5 })),
        ("ui_language_set", json!({})),
        ("locale_format", json!({ "language": "en" })),
        ("locale_format", json!({ "language": "auto", "integer": 1 })),
        ("locale_format", json!({ "number": 1, "decimals": 11 })),
        ("locale_format", json!({ "number": 1, "decimals": -1 })),
        ("locale_format", json!({ "date": "2026-13-01" })),
        ("locale_format", json!({ "date": "٢٠٢٦-١٠-٠٨" })),
        ("locale_format", json!({ "pdf_date": "D:2026" })),
        ("locale_format", json!({ "count": -1 })),
        ("prefs_set", json!({ "key": "general.language", "value": "de-AT" })),
    ] {
        assert!(a.call(tool, &args).is_err(), "{tool} {args} should be refused");
    }
    // A refused change keeps the language.
    assert_eq!(ok(&mut a, "ui_language_get", json!({}))["preference"], "ja");
}
