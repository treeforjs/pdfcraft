//! Permission checks fail closed (print, change, copy-derived field names) on synthetic,
//! original documents created through the tool table.
use pdfcraft_automation::{Automation, Content};
use serde_json::{Value, json};

fn call(a: &mut Automation, tool: &str, args: Value) -> Value {
    let result = a.call(tool, &args).unwrap_or_else(|e| panic!("{tool} {args}: {e}"));
    result
        .iter()
        .find_map(|c| match c {
            Content::Json(v) => Some(v.clone()),
            _ => None,
        })
        .unwrap_or(Value::Null)
}

fn setup(name: &str) -> (Automation, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("pdfcraft-guards-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let a = Automation::new().with_root(&root).unwrap();
    (a, root)
}

/// A printed form whose labels are confidential text; saved as `file` with copying denied
/// (changes allowed, printing as given).
fn protected_form(a: &mut Automation, file: &str, printing: &str, changes: &str) {
    let text = "Quokka zone: ____________________\n\nXylo code: ____________________";
    let doc = call(a, "doc_create", json!({ "from": "text", "text": text }))["doc"].as_u64().unwrap();
    call(
        a,
        "doc_protect",
        json!({ "doc": doc, "open_password": "Guard-user", "permissions_password": "Guard-owner", "copy": false, "printing": printing, "changes": changes }),
    );
    call(a, "doc_save", json!({ "doc": doc, "path": file }));
    call(a, "doc_close", json!({ "doc": doc, "discard_changes": true }));
}

/// Field names come from label words (page text): without copy permission detection still
/// proposes and adds the same fields, named Text1, Text2…; the owner password lifts it.
#[test]
fn detected_field_names_withhold_label_text_when_copying_is_denied() {
    let (mut a, root) = setup("detect");
    protected_form(&mut a, "form.pdf", "high", "any-except-extract");
    let user = call(&mut a, "doc_open", json!({ "path": "form.pdf", "password": "Guard-user" }))["doc"].as_u64().unwrap();
    let proposed = call(&mut a, "form_detect_fields", json!({ "doc": user, "add": false }));
    let names: Vec<&str> = proposed["fields"].as_array().unwrap().iter().map(|f| f["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Text1", "Text2"], "{proposed}");
    assert!(proposed["names_withheld"].as_str().is_some_and(|w| w.contains("does not allow copying")), "{proposed}");
    for leaked in ["Quokka", "zone", "Xylo", "code"] {
        assert!(!proposed.to_string().contains(leaked), "detection leaked {leaked:?}: {proposed}");
    }
    call(&mut a, "form_detect_fields", json!({ "doc": user }));
    let fields = call(&mut a, "form_fields", json!({ "doc": user }));
    assert_eq!(fields["fields"].as_array().unwrap().len(), 2, "{fields}");
    assert!(!fields.to_string().contains("Quokka") && !fields.to_string().contains("Xylo"), "{fields}");

    let owner = call(&mut a, "doc_open", json!({ "path": "form.pdf", "password": "Guard-owner" }))["doc"].as_u64().unwrap();
    let proposed = call(&mut a, "form_detect_fields", json!({ "doc": owner, "add": false }));
    let names: Vec<&str> = proposed["fields"].as_array().unwrap().iter().map(|f| f["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Quokka zone", "Xylo code"], "{proposed}");
    assert!(proposed.get("names_withheld").is_none());
    let _ = std::fs::remove_dir_all(root);
}

/// Make the plaintext `/P` of the encryption dictionary grant everything (-4), as an attacker
/// editing the file would. AES-256 binds `/P` to `/Perms`, so the engine must refuse the file.
fn grant_everything_in_plaintext_p(bytes: &mut [u8]) {
    let at = bytes.windows(3).position(|w| w[0] == b'/' && w[1] == b'P' && (w[2] == b' ' || w[2] == b'-')).expect("/P in the encryption dictionary");
    let start = at + 2 + bytes[at + 2..].iter().take_while(|b| **b == b' ').count();
    let end = start + 1 + bytes[start + 1..].iter().take_while(|b| b.is_ascii_digit()).count();
    let old: i32 = std::str::from_utf8(&bytes[start..end]).unwrap().parse().unwrap();
    assert_ne!(old, -4, "the fixture must restrict something");
    bytes[start..end].fill(b' ');
    bytes[start..start + 2].copy_from_slice(b"-4");
}

/// Printing and changes are refused for a restricted user and allowed for the owner; a file
/// whose `/P` was edited to grant them is not opened at all (with either password), so no
/// print or modify path can fall back to an unrestricted document.
#[test]
fn print_and_modify_fail_closed_on_restricted_and_tampered_permissions() {
    let (mut a, root) = setup("tamper");
    protected_form(&mut a, "locked.pdf", "none", "none");
    let user = call(&mut a, "doc_open", json!({ "path": "locked.pdf", "password": "Guard-user" }))["doc"].as_u64().unwrap();
    let security = call(&mut a, "doc_info", json!({ "doc": user }))["security"].clone();
    assert_eq!((security["printing"].as_str(), security["modify"].as_bool()), (Some("none"), Some(false)), "{security}");
    let error = a.call("doc_print", &json!({ "doc": user, "path": "user-print.pdf" })).unwrap_err().to_string();
    assert!(error.contains("printing"), "{error}");
    assert!(!root.join("user-print.pdf").exists());
    assert!(a.call("doc_set_info", &json!({ "doc": user, "key": "Title", "value": "changed" })).is_err());
    assert!(a.call("page_rotate", &json!({ "doc": user, "pages": [1], "degrees": 90 })).is_err());
    assert_eq!(call(&mut a, "doc_info", json!({ "doc": user }))["document"]["dirty"], json!(false));

    let owner = call(&mut a, "doc_open", json!({ "path": "locked.pdf", "password": "Guard-owner" }))["doc"].as_u64().unwrap();
    call(&mut a, "doc_print", json!({ "doc": owner, "path": "owner-print.pdf" }));
    assert!(root.join("owner-print.pdf").exists());
    call(&mut a, "doc_set_info", json!({ "doc": owner, "key": "Title", "value": "changed" }));

    let mut bytes = std::fs::read(root.join("locked.pdf")).unwrap();
    grant_everything_in_plaintext_p(&mut bytes);
    std::fs::write(root.join("tampered.pdf"), &bytes).unwrap();
    for password in ["Guard-user", "Guard-owner"] {
        let error = a.call("doc_open", &json!({ "path": "tampered.pdf", "password": password })).unwrap_err().to_string();
        assert!(error.contains("security settings could not be validated"), "{password}: {error}");
    }
    let docs = call(&mut a, "doc_list", json!({}));
    assert!(!docs.to_string().contains("tampered.pdf"), "a refused file never enters the session: {docs}");
    let _ = std::fs::remove_dir_all(root);
}
