//! Automation proves document/evidence persistence and explicit authority choices.
use pdfcraft_automation::{Automation, Content};
use serde_json::{Value, json};

#[path = "../../sign/tests/support/mod.rs"]
mod support;

fn call(a: &mut Automation, name: &str, args: Value) -> Value {
    match a.call(name, &args).unwrap().remove(0) {
        Content::Json(value) => value,
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn sign_evidence_reopen_and_selected_time_tools_preserve_trust_and_signed_prefix() {
    let dir = std::env::temp_dir().join(format!("pdfcraft-signature-automation-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (issuer, leaf) = support::chain();
    for (path, bytes) in [
        ("source.pdf", support::pdf()),
        ("id.p12", pdfcraft_sign::pkcs12::write(&leaf, "test").unwrap()),
        ("root.der", issuer.certificate.raw.clone()),
        ("good.crl", support::crl(&issuer, None)),
        ("good.ocsp", support::ocsp(&issuer, &leaf.certificate)),
    ] {
        std::fs::write(dir.join(path), bytes).unwrap();
    }
    let mut a = Automation::new().with_root(&dir).unwrap().with_clock(|| pdfcraft_sign::Time::from_pdf("20261012120000Z").unwrap().unix());
    let doc = call(&mut a, "doc_open", json!({"path":"source.pdf"}))["doc"].as_u64().unwrap();
    call(&mut a, "sign_trust", json!({"paths":["root.der"]}));
    call(&mut a, "sign_document", json!({"doc":doc,"id":"id.p12","password":"test","out":"signed.pdf"}));
    let signed = std::fs::read(dir.join("signed.pdf")).unwrap();
    let saved = call(
        &mut a,
        "sign_embed_evidence",
        json!({"doc":doc,"out":"evidence.pdf","certificates":["root.der"],"crls":["good.crl"],"ocsps":["good.ocsp"]}),
    );
    assert_eq!(saved["evidence"]["complete_revocation_coverage"], true);
    assert!(std::fs::read(dir.join("evidence.pdf")).unwrap().starts_with(&signed));
    let reopened = call(&mut a, "doc_open", json!({"path":"evidence.pdf"}))["doc"].as_u64().unwrap();
    let validation = call(&mut a, "sign_verify", json!({"doc":reopened,"verification_time":"20261012120000Z"}));
    assert_eq!(validation["signatures"][0]["status"], "valid");
    assert_eq!(validation["signatures"][0]["verification_time_source"], "chosen_time");
    call(&mut a, "sign_trust", json!({"clear":true}));
    assert_eq!(call(&mut a, "sign_list", json!({"doc":reopened}))["signatures"][0]["status"], "unknown");
    let denied = a.call(
        "sign_timestamp",
        &json!({"doc":doc,"out":"denied.pdf","timestamp_server":{
        "name":"No request permitted","url":"http://127.0.0.1:1/unused","timeout_seconds":1,"require_trusted_authority":true}}),
    );
    assert!(denied.unwrap_err().to_string().contains("allow_network"));
    assert!(!dir.join("denied.pdf").exists());
    assert!(a.call("sign_verify", &json!({"doc":doc,"verification_time":"20260230000000Z"})).is_err());
    assert!(a.call("sign_embed_evidence", &json!({"doc":doc,"out":"bad.pdf","crls":["../outside-root.crl"]})).is_err());
    assert!(!dir.join("bad.pdf").exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn automation_signature_timestamp_and_document_timestamp_use_opted_in_local_server() {
    let dir = std::env::temp_dir().join(format!("pdfcraft-timestamp-automation-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (_, leaf) = support::chain();
    let (url, cert, worker) = support::local_timestamp_server();
    std::fs::write(dir.join("source.pdf"), support::pdf()).unwrap();
    std::fs::write(dir.join("tsa.der"), cert.raw).unwrap();
    std::fs::write(dir.join("id.p12"), pdfcraft_sign::pkcs12::write(&leaf, "test").unwrap()).unwrap();
    let mut a = Automation::new().with_root(&dir).unwrap().with_clock(|| pdfcraft_sign::Time::from_pdf("20261012120000Z").unwrap().unix());
    let doc = call(&mut a, "doc_open", json!({"path":"source.pdf"}))["doc"].as_u64().unwrap();
    call(&mut a, "sign_trust", json!({"paths":["tsa.der"]}));
    let server = |url| json!({"name":"Local synthetic","url":url,"policy":"1.2.3.4","timeout_seconds":3,"require_trusted_authority":true});
    let signed = call(
        &mut a,
        "sign_document",
        json!({"doc":doc,"id":"id.p12","password":"test","out":"signed.pdf","timestamp_server":server(url),"allow_network":true}),
    );
    worker.join().unwrap();
    assert_eq!(signed["signature"]["verification_time_source"], "trusted_timestamp");
    assert_eq!(signed["signature"]["status"], "unknown", "trusting a TSA does not trust the document signer");
    let (url, _, worker) = support::local_timestamp_server();
    let stamped = call(&mut a, "sign_timestamp", json!({"doc":doc,"out":"stamped.pdf","timestamp_server":server(url),"allow_network":true}));
    worker.join().unwrap();
    assert_eq!(stamped["validation"]["signed"], 2);
    assert!(std::fs::read(dir.join("stamped.pdf")).unwrap().starts_with(&std::fs::read(dir.join("signed.pdf")).unwrap()));
    std::fs::remove_dir_all(dir).unwrap();
}
