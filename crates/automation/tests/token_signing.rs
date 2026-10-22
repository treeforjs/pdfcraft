//! Signing with smart cards and tokens (sign.pkcs11) through the tools, on the software token.

use std::path::PathBuf;
use std::sync::Arc;

use pdfcraft_automation::{Automation, Content, ToolError};
use pdfcraft_sign::token::mock::{Hostile, MockModule, MockProvider, MockToken};
use serde_json::{Value, json};

const MODULE: &str = "/opt/mock/libmocktoken.so";

fn sign_data(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/../sign/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn page() -> Vec<u8> {
    let objs: Vec<&[u8]> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] >>",
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

fn workdir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pdfcraft-token-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.pdf"), page()).unwrap();
    std::fs::write(dir.join("ec.p12"), sign_data("ec-p256.p12")).unwrap();
    dir
}

fn provider(hostile: Hostile) -> Arc<MockProvider> {
    let ec = pdfcraft_sign::pkcs12::open(&sign_data("ec-p256.p12"), "test").unwrap();
    let lonely = pdfcraft_sign::pkcs12::open(&sign_data("rsa-aes.p12"), "test").unwrap();
    let card = MockToken::new("Office Card", "4321")
        .with_certificate(b"\x01", "Signature", &ec.certificate.raw)
        .with_key(b"\x01", ec.key)
        .with_certificate(b"\x02", "Authentication", &lonely.certificate.raw);
    Arc::new(MockProvider::new().with_module(MODULE, MockModule::new().with_slot(0, None).with_slot(3, Some(card)).hostile(hostile)))
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

#[test]
fn tokens_are_listed_unlocked_and_sign_through_the_tools() {
    let dir = workdir("sign");
    let p = provider(Hostile::default());
    let mut a = Automation::new().with_root(&dir).unwrap().with_token_provider(p.clone());

    let modules = ok(&mut a, "sign_token_modules", json!({}));
    assert_eq!(modules["modules"], json!([MODULE]));
    assert_eq!(modules["available"], true);

    let slots = ok(&mut a, "sign_token_slots", json!({}));
    assert_eq!(slots["module"], MODULE);
    let list = slots["slots"].as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert!(list[0]["token"].is_null());
    assert_eq!(list[1]["slot"], 3);
    assert_eq!(list[1]["token"]["label"], "Office Card");
    assert_eq!(list[1]["token"]["login_required"], true);

    // A wrong PIN is the caller's to fix, and the error doesn't echo it.
    match a.call("sign_token_ids", &json!({ "pin": "9999" })) {
        Err(ToolError::InvalidArgs(m)) => assert!(m.contains("PIN is incorrect") && !m.contains("9999"), "{m}"),
        other => panic!("{other:?}"),
    }
    let ids = ok(&mut a, "sign_token_ids", json!({ "slot": 3, "pin": "4321" }));
    assert_eq!(ids["count"], 1);
    assert_eq!(ids["skipped"], json!([{ "label": "Authentication", "reason": "no private key on the token" }]));
    let reference = ids["ids"][0]["id"].as_str().unwrap().to_string();
    assert!(reference.starts_with("pkcs11:"));

    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    assert!(matches!(
        a.call("sign_document", &json!({ "doc": doc, "id": reference, "pin": "0000", "out": "bad.pdf" })),
        Err(ToolError::InvalidArgs(_))
    ));
    assert!(!dir.join("bad.pdf").exists());
    let signed = ok(&mut a, "sign_document", json!({ "doc": doc, "id": reference, "module": MODULE, "pin": "4321", "out": "signed.pdf" }));
    assert_eq!(signed["signature"]["signer"], ids["ids"][0]["certificate"]["name"]);
    assert!(dir.join("signed.pdf").exists());
    assert_eq!(p.module(MODULE).unwrap().counts().1, 1, "signed on the token");
    // Trusting the certificate makes the token's signature valid.
    ok(&mut a, "sign_trust", json!({ "paths": ["ec.p12"], "password": "test" }));
    assert_eq!(ok(&mut a, "sign_list", json!({ "doc": doc }))["signatures"][0]["status"], "valid");
    // An identity the token doesn't have.
    assert!(matches!(
        a.call("sign_document", &json!({ "doc": doc, "id": "pkcs11:Nobody", "pin": "4321", "out": "n.pdf" })),
        Err(ToolError::InvalidArgs(_))
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_modules_locked_tokens_and_hostile_modules() {
    let dir = workdir("hostile");
    // No modules at all.
    let mut a = Automation::new().with_root(&dir).unwrap().with_token_provider(Arc::new(MockProvider::new()));
    assert_eq!(ok(&mut a, "sign_token_modules", json!({}))["count"], 0);
    assert!(matches!(a.call("sign_token_slots", &json!({})), Err(ToolError::Failed(m)) if m.contains("no PKCS #11 module")));
    // With a root, a module path outside it that wasn't found is refused before loading.
    assert!(matches!(a.call("sign_token_slots", &json!({ "module": "/tmp/../etc/evil.so" })), Err(ToolError::Failed(m)) if m.contains("outside")));
    // A browser-like build: not available.
    let mut a = Automation::new().with_root(&dir).unwrap().with_token_provider(Arc::new(pdfcraft_sign::token::Unavailable));
    assert_eq!(ok(&mut a, "sign_token_modules", json!({}))["available"], false);

    // Three wrong PINs lock the token; then even the right one isn't tried.
    let p = provider(Hostile::default());
    let mut a = Automation::new().with_root(&dir).unwrap().with_token_provider(p.clone());
    for _ in 0..3 {
        assert!(a.call("sign_token_ids", &json!({ "pin": "1" })).is_err());
    }
    match a.call("sign_token_ids", &json!({ "pin": "4321" })) {
        Err(ToolError::Failed(m)) => assert!(m.contains("locked"), "{m}"),
        other => panic!("{other:?}"),
    }
    let slots = ok(&mut a, "sign_token_slots", json!({}));
    assert_eq!(slots["slots"][1]["token"]["pin_locked"], true);
    assert!(matches!(a.call("sign_token_ids", &json!({ "slot": -1, "pin": "1" })), Err(ToolError::InvalidArgs(_))));
    assert!(matches!(a.call("sign_token_ids", &json!({ "slot": 99, "pin": "1" })), Err(ToolError::InvalidArgs(_))));

    // A module that returns garbage signatures: nothing is written.
    let p = provider(Hostile { garbage_signature: true, slot_flood: 500, ..Hostile::default() });
    let mut a = Automation::new().with_root(&dir).unwrap().with_token_provider(p);
    assert!(ok(&mut a, "sign_token_slots", json!({}))["slots"].as_array().unwrap().len() <= 64);
    let reference = ok(&mut a, "sign_token_ids", json!({ "slot": 3, "pin": "4321" }))["ids"][0]["id"].as_str().unwrap().to_string();
    let doc = ok(&mut a, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    match a.call("sign_document", &json!({ "doc": doc, "id": reference, "slot": 3, "pin": "4321", "out": "g.pdf" })) {
        Err(ToolError::Failed(m)) => assert!(m.contains("unusable"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(!dir.join("g.pdf").exists());
    let _ = std::fs::remove_dir_all(&dir);
}
