//! Digital ID files as bytes (sign.web-signing): `sign_id_inspect` opens a .p12 given inline the
//! way the web app gets a file picked in the browser, or by path, and keeps neither the bytes nor
//! the password. Hostile input is an error, never a panic.

use base64::Engine as _;
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

fn root(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("pdfcraft-web-signing-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[test]
fn inspects_ids_given_as_bytes_or_paths() {
    let dir = root("inspect");
    let mut a = Automation::new().with_root(&dir).unwrap().with_clock(|| 1_800_000_000);
    for (key, describe, digest) in [("p256", "ECDSA P-256", "SHA-256"), ("rsa2048", "RSA 2048-bit", "SHA-256")] {
        let file = format!("{key}.p12");
        ok(&mut a, "sign_id_create", json!({ "name": "Ada Lovelace", "password": "secret1", "path": file, "key": key }));
        let bytes = std::fs::read(dir.join(&file)).unwrap();
        let v = ok(&mut a, "sign_id_inspect", json!({ "data_base64": b64(&bytes), "password": "secret1", "name": "C:\\Users\\ada\\ada.p12" }));
        assert_eq!(v["certificate"]["name"], "Ada Lovelace", "{v}");
        assert_eq!((v["key"].as_str(), v["digest"].as_str()), (Some(describe), Some(digest)), "{v}");
        assert_eq!((v["signs_in_browser"].as_bool(), v["valid_now"].as_bool()), (Some(true), Some(true)), "{v}");
        assert_eq!(v["name"], "ada.p12");
        assert_eq!(v["bytes"], bytes.len());
        // Wrapped base64 (as some clients send it) reads the same.
        let wrapped: String = b64(&bytes).as_bytes().chunks(64).map(|c| format!("{}\r\n", std::str::from_utf8(c).unwrap())).collect();
        assert_eq!(ok(&mut a, "sign_id_inspect", json!({ "data_base64": wrapped, "password": "secret1" }))["certificate"], v["certificate"]);
        assert_eq!(ok(&mut a, "sign_id_inspect", json!({ "path": file, "password": "secret1" }))["certificate"], v["certificate"]);
        assert!(err(&mut a, "sign_id_inspect", json!({ "data_base64": b64(&bytes), "password": "nope" })).contains("password is incorrect"));
    }
    // Outside its validity period by the session clock.
    let mut later = Automation::new().with_root(&dir).unwrap().with_clock(|| 2_100_000_000);
    let bytes = std::fs::read(dir.join("p256.p12")).unwrap();
    assert_eq!(ok(&mut later, "sign_id_inspect", json!({ "data_base64": b64(&bytes), "password": "secret1" }))["valid_now"], false);
}

#[test]
fn hostile_id_files_are_errors() {
    let dir = root("hostile");
    let mut a = Automation::new().with_root(&dir).unwrap();
    ok(&mut a, "sign_id_create", json!({ "name": "Grace Hopper", "password": "secret1", "path": "id.p12", "key": "p256" }));
    let good = std::fs::read(dir.join("id.p12")).unwrap();
    std::fs::write(dir.join("huge.p12"), vec![0x30u8; 1024 * 1024 + 1]).unwrap();
    let cases = [
        json!({}),
        json!({ "data_base64": b64(&good), "path": "id.p12" }),
        json!({ "data_base64": "not base64!!" }),
        json!({ "data_base64": "" }),
        json!({ "data_base64": "A".repeat(2 * 1024 * 1024) }),
        json!({ "data_base64": b64(b"PK\x03\x04 not a digital id") }),
        json!({ "data_base64": b64(&good[..good.len() / 2]), "password": "secret1" }),
        json!({ "data_base64": b64(b"\x30\x84\xff\xff\xff\xff"), "password": "secret1" }),
        json!({ "path": "huge.p12" }),
        json!({ "path": "missing.p12" }),
        json!({ "path": "../outside.p12" }),
        json!({ "data_base64": 5 }),
    ];
    for args in cases {
        err(&mut a, "sign_id_inspect", args);
    }
    // Damaged copies: every outcome is an answer, not a crash.
    let mut flipped = good.clone();
    for i in (0..flipped.len()).step_by(53) {
        flipped[i] ^= 0xA5;
        let _ = a.call("sign_id_inspect", &json!({ "data_base64": b64(&flipped), "password": "secret1" }));
    }
}
