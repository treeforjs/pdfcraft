//! sign.os-trust end to end: a signature by a test CA nobody trusts is Unknown; with the CA in an
//! injected "operating system" store it stays Unknown until the matching setting is on, then it is
//! Valid and reports the operating system as its trust source. Certified documents have their own
//! setting. Never reads the host's real store.
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

fn signature(a: &mut Automation, doc: u64) -> Value {
    let list = call(a, "sign_list", json!({ "doc": doc }));
    list["signatures"].as_array().unwrap().iter().find(|s| s["signed"] == true).unwrap().clone()
}

fn details(s: &Value) -> String {
    s["details"].as_array().unwrap().iter().map(|d| d.as_str().unwrap()).collect::<Vec<_>>().join("\n")
}

#[test]
fn os_roots_anchor_signatures_and_certified_documents_only_when_switched_on() {
    let dir = std::env::temp_dir().join(format!("pdfcraft-os-trust-automation-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("store")).unwrap();
    let (issuer, leaf) = support::chain();
    let root_pem = pdfcraft_sign::x509::to_pem(&issuer.certificate);
    // The fake operating-system bundle: the test CA among junk the loader must skip.
    std::fs::write(dir.join("ca-bundle.pem"), format!("junk\n-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n{root_pem}")).unwrap();
    std::fs::write(dir.join("store/root.pem"), &root_pem).unwrap();
    std::fs::write(dir.join("store/broken.der"), [0x30, 0x03, 0x01]).unwrap();
    std::fs::write(dir.join("source.pdf"), support::pdf()).unwrap();
    std::fs::write(dir.join("id.p12"), pdfcraft_sign::pkcs12::write(&leaf, "test").unwrap()).unwrap();
    std::fs::write(dir.join("root.der"), &issuer.certificate.raw).unwrap();
    let mut a = Automation::new().with_root(&dir).unwrap().with_clock(|| pdfcraft_sign::Time::from_pdf("20261012120000Z").unwrap().unix());

    // Off by default.
    let initial = call(&mut a, "sign_os_trust_get", json!({}));
    assert_eq!(initial, json!({ "signatures": false, "certified_documents": false, "certified_documents_effective": false, "source": "system" }));

    let approval = call(&mut a, "doc_open", json!({ "path": "source.pdf" }))["doc"].as_u64().unwrap();
    call(&mut a, "sign_document", json!({ "doc": approval, "id": "id.p12", "password": "test", "out": "signed.pdf" }));
    let certified = call(&mut a, "doc_open", json!({ "path": "source.pdf" }))["doc"].as_u64().unwrap();
    call(&mut a, "sign_document", json!({ "doc": certified, "id": "id.p12", "password": "test", "out": "certified.pdf", "certify": "no_changes" }));
    for doc in [approval, certified] {
        let s = signature(&mut a, doc);
        assert_eq!(s["status"], "unknown");
        assert_eq!(s["trust_source"], Value::Null);
    }

    // Injecting the store reads it, but trusts nothing yet; validation says why.
    let set = call(&mut a, "sign_os_trust_set", json!({ "source": "bundle", "path": "ca-bundle.pem" }));
    assert_eq!(set["signatures"], false);
    assert!(set["source"].as_str().unwrap().starts_with("bundle:"));
    let s = signature(&mut a, approval);
    assert_eq!(s["status"], "unknown");
    assert!(details(&s).contains("not trusted for validating signatures"), "{}", details(&s));

    let roots = call(&mut a, "sign_os_roots", json!({}));
    assert_eq!(roots["count"], 1);
    assert_eq!(roots["skipped"], 1);
    assert_eq!(roots["supported"], true);
    assert_eq!(roots["roots"][0]["sha256"], issuer.certificate.fingerprint());
    assert_eq!(call(&mut a, "sign_os_roots", json!({ "query": "evidence root" }))["matching"], 1);
    assert_eq!(call(&mut a, "sign_os_roots", json!({ "query": "no such root" }))["roots"], json!([]));
    assert!(a.call("sign_os_roots", &json!({ "limit": 0 })).is_err());
    assert!(a.call("sign_os_roots", &json!({ "offset": -1 })).is_err());

    // For signatures: the approval signature is valid, anchored by the operating system; the
    // certification signature is not, until the certified-documents setting is on too.
    let on = call(&mut a, "sign_os_trust_set", json!({ "signatures": true }));
    assert_eq!((on["roots"].clone(), on["skipped"].clone()), (json!(1), json!(1)));
    let s = signature(&mut a, approval);
    assert_eq!(s["status"], "valid", "{}", details(&s));
    assert_eq!(s["trust_source"], "operating_system");
    assert!(details(&s).contains("operating system's certificate store"));
    let c = signature(&mut a, certified);
    assert_eq!(c["certify"], 1);
    assert_eq!(c["status"], "unknown");
    assert_eq!(c["trust_source"], Value::Null);
    assert!(details(&c).contains("not trusted for validating certified documents"), "{}", details(&c));

    let both = call(&mut a, "sign_os_trust_set", json!({ "certified_documents": true }));
    assert_eq!(both["certified_documents_effective"], true);
    let c = signature(&mut a, certified);
    assert_eq!(c["status"], "valid", "{}", details(&c));
    assert_eq!(c["trust_source"], "operating_system");

    // Certified documents alone is not enough (PDF greys it out without signatures).
    let only = call(&mut a, "sign_os_trust_set", json!({ "signatures": false }));
    assert_eq!((only["certified_documents"].clone(), only["certified_documents_effective"].clone()), (json!(true), json!(false)));
    assert_eq!(signature(&mut a, certified)["status"], "unknown");
    assert_eq!(signature(&mut a, approval)["status"], "unknown");

    // A directory store works the same, and a reopened file validates against it.
    call(&mut a, "sign_os_trust_set", json!({ "signatures": true, "source": "directory", "path": "store" }));
    let listed = call(&mut a, "sign_os_roots", json!({ "reload": true }));
    assert_eq!((listed["count"].clone(), listed["skipped"].clone()), (json!(1), json!(1)));
    let reopened = call(&mut a, "doc_open", json!({ "path": "signed.pdf" }))["doc"].as_u64().unwrap();
    assert_eq!(signature(&mut a, reopened)["trust_source"], "operating_system");

    // The user's own trusted certificates win over the operating system's roots.
    call(&mut a, "sign_trust", json!({ "paths": ["root.der"] }));
    assert_eq!(signature(&mut a, reopened)["trust_source"], "trusted_certificates");
    call(&mut a, "sign_trust", json!({ "clear": true }));

    // Bad arguments change nothing.
    for bad in [
        json!({ "source": "bundle" }),
        json!({ "source": "bundle", "path": "missing.pem", "signatures": false }),
        json!({ "source": "directory", "path": "../outside" }),
        json!({ "source": "keychain" }),
        json!({ "path": "store" }),
        json!({ "source": "system", "path": "store" }),
    ] {
        assert!(a.call("sign_os_trust_set", &bad).is_err(), "{bad}");
    }
    let after = call(&mut a, "sign_os_trust_get", json!({}));
    assert_eq!(after["signatures"], true);
    assert!(after["source"].as_str().unwrap().starts_with("directory:"));
    assert_eq!(signature(&mut a, reopened)["status"], "valid");

    // Off again: back to Unknown.
    call(&mut a, "sign_os_trust_set", json!({ "signatures": false, "certified_documents": false }));
    assert_eq!(signature(&mut a, reopened)["status"], "unknown");
    std::fs::remove_dir_all(dir).unwrap();
}
